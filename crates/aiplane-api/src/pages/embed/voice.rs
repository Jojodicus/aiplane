// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Voice in the widget (`docs/embed.md` → "Voice"): `POST
//! /api/v0/embed/transcribe` turns a recording into text the visitor reads
//! and sends themselves, `POST /api/v0/embed/speak` speaks a finished
//! answer, and `GET /api/v0/embed/recorder.js` is the audio worklet the
//! widget records with.
//!
//! Each direction exists only while the conversation's version switches it
//! on (`publish.voice`), and each call is gated like a message: the
//! visitor's rates and the owner's budget. A recording is held in memory
//! for the call and never stored; the transcript and the spoken text are
//! recorded in the conversation's activity chain
//! (`aiplane_runtime::agents::voice`).

use std::sync::Arc;

use aiplane_agents::db::system_principals;
use aiplane_core::server::principal::GrantKind;
use aiplane_features::server::vad;
use aiplane_runtime::agents::defaults::{self, VoiceDirection};
use aiplane_runtime::agents::spec::AgentSpec;
use aiplane_runtime::agents::spec::model::VoiceSpec;
use aiplane_runtime::agents::voice::{self, Conversation, Recording};
use aiplane_runtime::rama_server::state::RamaState;
use rama::bytes::Bytes;
use rama::http::service::web::extract::State;
use rama::http::service::web::response::IntoResponse;
use rama::http::{Request, Response, StatusCode, header};
use serde::Deserialize;
use session_core::chrome::{CappedBodyError, read_body_capped};
use session_core::db::{self as chat, TurnRole, TurnStatus};
use session_core::i18n::Lang;

use super::{MAX_BODY_BYTES, MAX_MESSAGE_CHARS, Visitor, admit, visitor};
use crate::pages::{bad_request, internal, json_error, json_ok, payload_too_large};

/// The largest recording read: a minute of 16 kHz mono 16-bit PCM is
/// 1.92 MB, plus the WAV header.
const MAX_RECORDING_BYTES: usize = 2 * 1024 * 1024;
const MAX_RECORDING_SECONDS: f64 = 60.0;
/// Shorter than this is a stray click, which a transcription model answers
/// with invented text or not at all.
const MIN_RECORDING_SECONDS: f64 = 0.4;

/// The audio worklet the widget records with: it hands each block of raw
/// samples to the page, which encodes them as WAV. The SPA's file
/// (`web/static/pcm-recorder.js`), served here too so it rides the embed CORS
/// a cross-origin worklet needs.
const RECORDER_JS: &str = include_str!("../../../../../web/static/pcm-recorder.js");

fn voice_off(direction: &str) -> Response {
    json_error(
        StatusCode::NOT_FOUND,
        "voice_not_enabled",
        &format!(
            "this assistant does not offer voice {direction} — its owner can switch it on with \
             `publish.voice.{direction}`"
        ),
    )
}

const TRANSCRIPTION_UNAVAILABLE: &str = "speech recognition is unavailable right now — type the \
     message instead, or try again in a moment";
const SPEECH_UNAVAILABLE: &str =
    "spoken answers are unavailable right now — the answer is on screen; try again in a moment";

fn voice_unavailable(message: &str) -> Response {
    json_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "voice_unavailable",
        message,
    )
}

/// The model a direction that is on runs on for this agent: the one its
/// spec names, else the gateway's default. When there is none, or the agent
/// holds no grant on it (an admin changed the defaults or the grants after
/// publishing), the visitor is told voice is unavailable.
async fn voice_target(
    state: &RamaState,
    principal: &aiplane_core::server::principal::SystemPrincipal,
    voice: &VoiceSpec,
    direction: VoiceDirection,
    message: &str,
) -> Result<String, Response> {
    let model = defaults::voice_model(state, voice, direction).await;
    match model {
        Some(model) if principal.grants.has(GrantKind::Model, &model) => Ok(model),
        model => {
            tracing::warn!(
                agent = %principal.id,
                ?direction,
                ?model,
                "the agent holds no grant on a model for this voice direction — name one it \
                 holds in publish.voice, or grant it the gateway's default model"
            );
            Err(voice_unavailable(message))
        }
    }
}

/// The agent's principal, with the grants its models are checked against.
async fn agent_principal(
    state: &RamaState,
    v: &Visitor,
) -> Result<aiplane_core::server::principal::SystemPrincipal, Response> {
    system_principals::load_active(&state.db, &v.session.principal_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| {
            json_error(
                StatusCode::FORBIDDEN,
                "agent_disabled",
                "this assistant has been switched off by its owner — try again later or use the \
                 website's other contact options",
            )
        })
}

/// POST /api/v0/embed/transcribe — a recording (`audio/wav`, 16 kHz mono
/// 16-bit PCM, 0.4–60 s) in, `{text}` out. The text is the visitor's to
/// read, change and send; nothing is posted to the conversation.
pub async fn transcribe(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let v = or_return!(visitor(&state, &req).await);
    let lang = Lang::from_request(req.headers());
    let ip = state.client_ip(&req);
    let spec = v.live.spec.agent().unwrap_or(AgentSpec::empty());
    if !spec.publish.voice.input {
        return voice_off("input");
    }
    let audio = match read_body_capped(req.into_body(), MAX_RECORDING_BYTES).await {
        Ok(bytes) => bytes,
        Err(CappedBodyError::TooLarge { max }) => return payload_too_large("the recording", max),
        Err(CappedBodyError::Read(e)) => return bad_request(e),
    };
    let Some(seconds) = vad::pcm16_mono_16k_duration_seconds(&audio) else {
        return json_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_audio",
            "the recording must be a WAV of 16 kHz mono 16-bit PCM — the widget's recorder \
             sends exactly that",
        );
    };
    if seconds > MAX_RECORDING_SECONDS {
        return json_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "audio_too_long",
            &format!(
                "the recording is {seconds:.0} s long; at most {MAX_RECORDING_SECONDS:.0} s are \
                 transcribed — record a shorter message"
            ),
        );
    }
    if seconds < MIN_RECORDING_SECONDS {
        return json_error(
            StatusCode::BAD_REQUEST,
            "audio_too_short",
            "the recording is too short — hold the microphone button while you speak",
        );
    }
    or_return!(
        admit(
            &state,
            &v.session.principal_id,
            Some(&v.session.id),
            ip.as_deref(),
            lang,
        )
        .await
    );
    let principal = or_return!(agent_principal(&state, &v).await);
    let target = or_return!(
        voice_target(
            &state,
            &principal,
            &spec.publish.voice,
            VoiceDirection::Input,
            TRANSCRIPTION_UNAVAILABLE,
        )
        .await
    );
    let wav: Bytes = vad::trim_silence(&audio).map_or(audio, |t| t.bytes);
    let at = Conversation {
        principal: &principal,
        version: v.live.version,
        session_id: &v.session.session_id,
        visitor_id: &v.session.id,
    };
    match voice::transcribe(&state, &target, &at, Recording { wav, seconds }).await {
        Ok(text) => {
            let text: String = text.chars().take(MAX_MESSAGE_CHARS).collect();
            json_ok(StatusCode::OK, Transcript { text })
        }
        Err(_) => voice_unavailable(TRANSCRIPTION_UNAVAILABLE),
    }
}

#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct Transcript {
    /// What the recording says, for the visitor to read and send.
    pub text: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpeakBody {
    /// A finished answer in the visitor's conversation.
    pub turn_id: String,
}

/// POST /api/v0/embed/speak — `{turn_id}` of a finished answer in the
/// visitor's conversation in, its spoken audio (`audio/mpeg`) out; `204`
/// when nothing of it is speakable. What is spoken is the answer as stored
/// once the output filter ruled on it, never text the client sends.
pub async fn speak(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let v = or_return!(visitor(&state, &req).await);
    let lang = Lang::from_request(req.headers());
    let ip = state.client_ip(&req);
    let spec = v.live.spec.agent().unwrap_or(AgentSpec::empty());
    let voice_spec = spec.publish.voice.clone();
    if !voice_spec.output {
        return voice_off("output");
    }
    let body: SpeakBody = or_return!(
        crate::pages::read_json_capped(req.into_body(), "the speak body", MAX_BODY_BYTES).await
    );
    let turn = match chat::get_turn(&state.db, &v.session.session_id, &body.turn_id).await {
        Ok(Some(turn)) => turn,
        Ok(None) => {
            return json_error(
                StatusCode::NOT_FOUND,
                "turn_not_found",
                "this conversation has no such answer — speak a `turn_id` from its events",
            );
        }
        Err(err) => return internal(err),
    };
    let held = state
        .chats
        .get(&v.session.principal_id, &v.session.session_id)
        .is_some_and(|w| w.turn_id == turn.id);
    let Some(answer) = turn
        .content
        .as_deref()
        .filter(|c| !c.trim().is_empty())
        .filter(|_| {
            turn.role == TurnRole::Assistant && turn.status == TurnStatus::Completed && !held
        })
    else {
        return json_error(
            StatusCode::CONFLICT,
            "turn_not_final",
            "only a finished answer can be spoken — wait for it on GET /api/v0/embed/events",
        );
    };
    or_return!(
        admit(
            &state,
            &v.session.principal_id,
            Some(&v.session.id),
            ip.as_deref(),
            lang,
        )
        .await
    );
    let principal = or_return!(agent_principal(&state, &v).await);
    let target = or_return!(
        voice_target(
            &state,
            &principal,
            &voice_spec,
            VoiceDirection::Output,
            SPEECH_UNAVAILABLE,
        )
        .await
    );
    let at = Conversation {
        principal: &principal,
        version: v.live.version,
        session_id: &v.session.session_id,
        visitor_id: &v.session.id,
    };
    match voice::speak(
        &state,
        &target,
        voice_spec.voice.as_deref(),
        &at,
        &turn.id,
        answer,
        lang,
    )
    .await
    {
        Ok(Some(audio)) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "audio/mpeg")
            .header(header::CACHE_CONTROL, "no-store")
            .body(audio.into())
            .unwrap_or_else(internal),
        Ok(None) => (StatusCode::NO_CONTENT, "").into_response(),
        Err(_) => voice_unavailable(SPEECH_UNAVAILABLE),
    }
}

/// GET /api/v0/embed/recorder.js — [`RECORDER_JS`].
pub async fn recorder() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/javascript")
        .header(header::CACHE_CONTROL, "public, max-age=300")
        .body(RECORDER_JS.into())
        .unwrap_or_else(internal)
}
