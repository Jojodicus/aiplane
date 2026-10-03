// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! `/api/v0/embed/*` — the public agent endpoint a website's widget talks to
//! (`docs/agents.md` §5).
//!
//! Nobody here is signed in. A visitor starts a session with an embed key
//! (`gwe_…`) from an allowlisted `Origin` and gets a visitor token (`gwv_…`)
//! back, which the widget keeps in `sessionStorage` and sends as
//! `Authorization: Bearer` on every later call. The token names exactly one
//! conversation, so no request carries a session id a visitor could change.
//! The token is accepted here and nowhere else: every other `/api/v0` route
//! wants a session cookie, `/v1/*` wants `gwk_`/`gws_`.
//!
//! Every request re-checks the whole chain — key live, origin listed by that
//! key, agent enabled and published — so revoking a key or disabling an
//! agent ends open conversations at their next request. Only the live
//! version is ever served.
//!
//! A visitor never sees tool calls, reasoning, upstream error text or an
//! answer still being written: each assistant answer arrives whole once its
//! turn is terminal (`OutputPolicy::Buffered`).

use std::sync::Arc;

use jiff::Timestamp;
use rama::http::service::web::extract::State;
use rama::http::{HeaderMap, HeaderValue, Request, Response, StatusCode, header};
use serde::Deserialize;
use serde_json::{Value, json};
use session_core::chat_json::{ChatEvent, SseTx, json_stream_response, sse_json};
use session_core::db::{self as chat, TurnRole, TurnStatus, TurnWithTools};
use session_core::i18n::{self, Lang, t, t_args};

use super::turn_wait::{TurnWait, Waited};
use super::{bad_request, internal, json_error, json_ok};
use aiplane_core::server::auth::token;
use aiplane_core::server::db::agents::{self as agents_db, AgentRow};
use aiplane_core::server::db::embed_keys::{self, EmbedKey};
use aiplane_core::server::db::visitor_sessions::{self, Lookup, NewVisitorSession, VisitorSession};
use aiplane_runtime::agents::embed::{
    self as embed_rt, Admission, OpenedTurn, Refusal, ReleaseWatch, TurnWork,
};
use aiplane_runtime::agents::resume::{
    AgentResume, AgentResumeError, ResumedBy, claim as claim_resume,
};
use aiplane_runtime::agents::spec::AgentSpec;
use aiplane_runtime::agents::spec_cache::CompiledSpec;
use aiplane_runtime::rama_server::state::RamaState;
use aiplane_runtime::suspend::ResumeRefused;

macro_rules! or_return {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(resp) => return resp,
        }
    };
}

/// Longest visitor message accepted, in characters.
const MAX_MESSAGE_CHARS: usize = 8_000;
/// Largest request body a public embed route reads. A message is capped at
/// [`MAX_MESSAGE_CHARS`]; this leaves room for JSON escaping and an identity
/// token, and stops an anonymous client from making the gateway buffer more.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// What a visitor reads when a turn errored, in their language — the same
/// Fluent message an A2A caller gets for a failed task. The real message (an
/// upstream status, a tool failure) is for the agent's owner, not an
/// anonymous visitor.
fn visitor_error(lang: Lang) -> String {
    t(lang, "embed-error-generic")
}

fn embed_key_invalid() -> Response {
    json_error(
        StatusCode::UNAUTHORIZED,
        "embed_key_invalid",
        "this embed key is not known to the gateway — copy the key from the agent's embed \
         settings into the widget snippet again",
    )
}

fn embed_key_revoked() -> Response {
    json_error(
        StatusCode::FORBIDDEN,
        "embed_key_revoked",
        "this website's embed key was revoked — the site owner needs to create a new key for \
         the agent and update the widget snippet",
    )
}

fn visitor_session_invalid() -> Response {
    json_error(
        StatusCode::UNAUTHORIZED,
        "visitor_session_invalid",
        "this request needs a visitor token in `Authorization: Bearer gwv_…` — start a \
         conversation with POST /api/v0/embed/sessions",
    )
}

fn visitor_session_expired() -> Response {
    json_error(
        StatusCode::UNAUTHORIZED,
        "visitor_session_expired",
        "this conversation ended after a period without activity — start a new one with POST \
         /api/v0/embed/sessions",
    )
}

/// The request's `Origin`, which must be one the key lists and, when the
/// conversation's version sets `publish.origins`, one listed there too. A
/// browser always sends it on the widget's cross-origin calls. A version
/// whose spec does not read allows no origin.
fn check_origin(key: &EmbedKey, spec: &CompiledSpec, headers: &HeaderMap) -> Result<(), Response> {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .filter(|o| !o.is_empty());
    let Some(origin) = origin else {
        return Err(json_error(
            StatusCode::FORBIDDEN,
            "origin_not_allowed",
            "the request carries no `Origin` header — the agent can only be used from a web page \
             on a website its embed key lists",
        ));
    };
    if !key.allows(origin) {
        return Err(json_error(
            StatusCode::FORBIDDEN,
            "origin_not_allowed",
            &format!(
                "`{origin}` may not embed this agent — the site owner needs to add exactly \
                 `{origin}` to the embed key's origins"
            ),
        ));
    }
    if spec.agent().is_ok_and(|a| a.publish.allows_origin(origin)) {
        return Ok(());
    }
    Err(json_error(
        StatusCode::FORBIDDEN,
        "origin_not_allowed",
        &format!(
            "`{origin}` is on the embed key but not in the agent's published \
             `publish.origins` — the agent's owner needs to add it there and publish again"
        ),
    ))
}

/// The agent as a visitor's conversation runs it: enabled, published, on the
/// version the conversation is pinned to (the live one for a new
/// conversation).
struct Live {
    agent: AgentRow,
    version: i64,
    spec: Arc<CompiledSpec>,
}

async fn live_agent(
    state: &RamaState,
    principal_id: &str,
    pinned: Option<i64>,
) -> Result<Live, Response> {
    let Some(agent) = agents_db::get(&state.db, principal_id)
        .await
        .map_err(internal)?
    else {
        return Err(embed_key_revoked());
    };
    if agent.principal.disabled_at.is_some() {
        return Err(json_error(
            StatusCode::FORBIDDEN,
            "agent_disabled",
            "this assistant has been switched off by its owner — try again later or use the \
             website's other contact options",
        ));
    }
    let not_published = || {
        json_error(
            StatusCode::CONFLICT,
            "agent_not_published",
            "this assistant has no published version yet — its owner needs to publish it before \
             a website can use it",
        )
    };
    let Some(version) = pinned.or(agent.live_version) else {
        return Err(not_published());
    };
    let Some(spec) = state
        .agent_specs
        .version(&state.db, principal_id, version)
        .await
        .map_err(internal)?
    else {
        return Err(not_published());
    };
    Ok(Live {
        agent,
        version,
        spec,
    })
}

/// Gate a request that makes the agent work — a new conversation or a
/// message — on the agent's visitor rates and budget (`docs/agents.md` §5,
/// "What #92 built"). Reads cost the agent nothing and are not gated: the
/// widget re-attaches to its event stream whenever it likes.
async fn admit(
    state: &RamaState,
    agent_id: &str,
    visitor_id: Option<&str>,
    ip: Option<&str>,
    lang: Lang,
) -> Result<(), Response> {
    let who = Admission {
        visitor_id,
        a2a_context: None,
        ip,
    };
    match embed_rt::admit(state, agent_id, who, Timestamp::now()).await {
        Ok(()) => Ok(()),
        Err(refusal) => Err(refused(&refusal, lang)),
    }
}

/// A refusal in the visitor's language. The visitor learns to wait, or that
/// the assistant is unavailable; why it is unavailable (the owner's budget)
/// is for the agent's managers, who find it in the audit trail and in
/// `GET /api/v0/agents/{id}` under `limits`.
fn refused(refusal: &Refusal, lang: Lang) -> Response {
    let retry = refusal.retry_after_secs();
    let mut resp = match refusal {
        Refusal::Rate(_) => json_error(
            StatusCode::TOO_MANY_REQUESTS,
            "visitor_rate_limited",
            &t_args(
                lang,
                "agent-embed-rate-limited",
                &i18n::args([("seconds", retry.into())]),
            ),
        ),
        Refusal::Budget(_) => json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "agent_unavailable",
            &t(lang, "agent-embed-unavailable"),
        ),
    };
    resp.headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(retry));
    resp
}

fn agent_json(live: &Live) -> Value {
    json!({ "display": live.agent.principal.display })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartBody {
    pub key: String,
}

/// POST /api/v0/embed/sessions — start a visitor conversation.
pub async fn start_session(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let ip = state.client_ip(&req);
    let headers = req.headers().clone();
    let body: StartBody = or_return!(
        super::read_json_capped(req.into_body(), "the session body", MAX_BODY_BYTES).await
    );
    let Some(key_hash) = token::hash_embed_key(body.key.trim()) else {
        return embed_key_invalid();
    };
    let key = match embed_keys::find_by_hash(&state.db, &key_hash).await {
        Ok(Some(k)) => k,
        Ok(None) => return embed_key_invalid(),
        Err(err) => return internal(err),
    };
    if key.revoked_at.is_some() {
        return embed_key_revoked();
    }
    let live = or_return!(live_agent(&state, &key.principal_id, None).await);
    or_return!(check_origin(&key, &live.spec, &headers));
    let lang = Lang::from_request(&headers);
    or_return!(admit(&state, &key.principal_id, None, ip.as_deref(), lang).await);

    let (visitor_token, token_hash) = token::mint_visitor();
    let idle_ttl = live
        .spec
        .agent()
        .unwrap_or(AgentSpec::empty())
        .publish
        .idle_ttl();
    let started = visitor_sessions::start(
        &state.db,
        &NewVisitorSession {
            principal_id: &key.principal_id,
            embed_key_id: &key.id,
            agent_version: live.version,
            token_hash: &token_hash,
            client_ip: ip.as_deref(),
            idle_ttl,
            max_age: embed_rt::MAX_VISITOR_SESSION,
            now: Timestamp::now(),
        },
    )
    .await;
    match started {
        Ok(v) => json_ok(
            StatusCode::CREATED,
            json!({
                "token": visitor_token,
                "expires_at": v.expires_at,
                "idle_ttl_secs": v.idle_ttl.as_secs(),
                "agent": agent_json(&live),
            }),
        ),
        Err(err) => internal(err),
    }
}

/// An accepted visitor request: its session (already slid) and the agent on
/// the conversation's version.
struct Visitor {
    session: VisitorSession,
    live: Live,
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let rest = value.strip_prefix("Bearer ")?.trim();
    (!rest.is_empty()).then_some(rest)
}

/// Resolve and check the visitor behind `req`. Only a request that passes
/// every check slides the session's expiry.
async fn visitor(state: &RamaState, req: &Request) -> Result<Visitor, Response> {
    let Some(token_hash) = bearer(req.headers()).and_then(token::hash_visitor_token) else {
        return Err(visitor_session_invalid());
    };
    let now = Timestamp::now();
    let session = match visitor_sessions::lookup(&state.db, &token_hash, now)
        .await
        .map_err(internal)?
    {
        Lookup::Active(s) => s,
        Lookup::Expired => return Err(visitor_session_expired()),
        Lookup::Unknown => return Err(visitor_session_invalid()),
    };
    let key = embed_keys::get(&state.db, &session.embed_key_id)
        .await
        .map_err(internal)?
        .filter(|k| k.revoked_at.is_none())
        .ok_or_else(embed_key_revoked)?;
    let pinned = chat::get_principal_session(&state.db, &session.principal_id, &session.session_id)
        .await
        .map_err(internal)?
        .and_then(|run| run.agent_version);
    let live = live_agent(state, &session.principal_id, pinned).await?;
    check_origin(&key, &live.spec, req.headers())?;
    let session = visitor_sessions::slide(&state.db, &session, now)
        .await
        .map_err(internal)?;
    Ok(Visitor { session, live })
}

/// A turn as a visitor may see it: the text of the exchange, and nothing of
/// how it was produced. A suspended turn keeps what it waits for, without
/// the tool, and offers the visitor only the decisions that are theirs.
fn visitor_turn(mut t: TurnWithTools, lang: Lang) -> TurnWithTools {
    t.tool_calls.clear();
    t.steers.clear();
    t.suspension = t
        .suspension
        .filter(|_| t.turn.status == TurnStatus::Suspended)
        .map(|s| s.for_participant());
    let turn = &mut t.turn;
    turn.model = None;
    turn.reasoning = None;
    turn.reasoning_elapsed_ms = None;
    turn.reasoning_started_at = None;
    if turn.role == TurnRole::Assistant {
        if !turn.status.is_terminal() {
            turn.content = None;
        }
        if turn.status == TurnStatus::Errored {
            turn.error_message = Some(visitor_error(lang));
        }
    }
    t
}

async fn visitor_turns(
    state: &RamaState,
    session_id: &str,
    lang: Lang,
) -> Result<Vec<TurnWithTools>, Response> {
    let mut turns = chat::list_turns(&state.db, session_id)
        .await
        .map_err(internal)?;
    if state.agent_turns.is_running(session_id)
        && let Some(last) = turns.last_mut()
        && last.turn.role == TurnRole::Assistant
    {
        // Terminal in the database, but the output filter has not ruled on
        // the answer yet.
        last.turn.status = TurnStatus::InProgress;
    }
    Ok(turns.into_iter().map(|t| visitor_turn(t, lang)).collect())
}

/// GET /api/v0/embed/session — the conversation behind a visitor token, for
/// a widget that reloaded and found its token in `sessionStorage`.
pub async fn current_session(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let v = or_return!(visitor(&state, &req).await);
    let lang = Lang::from_request(req.headers());
    let turns = or_return!(visitor_turns(&state, &v.session.session_id, lang).await);
    let live_turn_id = live_turn_id(&turns);
    json_ok(
        StatusCode::OK,
        json!({
            "expires_at": v.session.expires_at,
            "idle_ttl_secs": v.session.idle_ttl.as_secs(),
            "agent": agent_json(&v.live),
            "live_turn_id": live_turn_id,
            "turns": turns,
        }),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageBody {
    pub text: String,
}

/// POST /api/v0/embed/messages — the visitor's next message. 202 once its
/// turn runs; the answer comes through `GET /api/v0/embed/events`.
pub async fn send_message(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let v = or_return!(visitor(&state, &req).await);
    let lang = Lang::from_request(req.headers());
    let ip = state.client_ip(&req);
    let body: MessageBody = or_return!(
        super::read_json_capped(req.into_body(), "the message body", MAX_BODY_BYTES).await
    );
    let text = body.text.trim();
    if text.is_empty() {
        return bad_request("the message is empty — send `{\"text\": \"…\"}`");
    }
    if text.chars().count() > MAX_MESSAGE_CHARS {
        return bad_request(format!(
            "the message is longer than {MAX_MESSAGE_CHARS} characters — shorten it and send it \
             again"
        ));
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
    let Some(runner) = state.agent_turns.runner() else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "agent_runtime_unavailable",
            "this gateway cannot run agent conversations yet — the message was not stored; try \
             again after the gateway has been updated",
        );
    };
    let session_id = v.session.session_id.clone();
    let turn_in_progress = || {
        json_error(
            StatusCode::CONFLICT,
            "turn_in_progress",
            "the assistant is still answering the previous message — wait for its answer on \
             GET /api/v0/embed/events, then send this one",
        )
    };
    let user_turn_id = uuid::Uuid::new_v4().to_string();
    let assistant_turn_id = uuid::Uuid::new_v4().to_string();
    let Some(claim) = state.agent_turns.claim(&session_id, &assistant_turn_id) else {
        return turn_in_progress();
    };
    match chat::in_flight_turn(&state.db, &session_id).await {
        Ok(Some(_)) => return turn_in_progress(),
        Ok(None) => {}
        Err(err) => return internal(err),
    }
    match chat::suspended_turn_in_session(&state.db, &session_id).await {
        Ok(Some(_)) => return queue_behind_decision(&state, &session_id, text, lang).await,
        Ok(None) => {}
        Err(err) => return internal(err),
    }

    let model = v.live.spec.main_pool().unwrap_or_default().to_string();
    if let Err(err) = chat::create_user_turn(&state.db, &session_id, &user_turn_id, text).await {
        return internal(err);
    }
    if let Err(err) =
        chat::create_assistant_turn_in_progress(&state.db, &session_id, &assistant_turn_id, &model)
            .await
    {
        return internal(err);
    }

    let turn = OpenedTurn {
        agent_id: v.session.principal_id.clone(),
        version: v.live.version,
        session_id: session_id.clone(),
        turn_id: assistant_turn_id.clone(),
        visitor_id: Some(v.session.id.clone()),
        caller: None,
        lang: Some(lang),
    };
    embed_rt::spawn_guarded(state, runner, claim, TurnWork::Run(turn));

    json_ok(
        StatusCode::ACCEPTED,
        json!({
            "turn_id": assistant_turn_id,
            "user_turn_id": user_turn_id,
            "placement": "started",
        }),
    )
}

/// The user turn waiting behind the conversation's pending decision, if the
/// visitor already sent one. It runs once the decision is settled.
fn queued_message(turns: &[TurnWithTools]) -> Option<String> {
    turns
        .last()
        .filter(|t| t.turn.role == TurnRole::User)
        .filter(|_| turns.iter().any(|t| t.turn.status == TurnStatus::Suspended))
        .map(|t| t.turn.id.clone())
}

/// A message sent while the conversation waits for a decision is stored and
/// runs after the decision, as the #96 decision on suspend/resume says — it
/// neither cancels the pending request nor answers it. One message waits at
/// a time.
async fn queue_behind_decision(
    state: &RamaState,
    session_id: &str,
    text: &str,
    lang: Lang,
) -> Response {
    match chat::turn_before(&state.db, session_id, i64::MAX).await {
        Ok(Some(last)) if last.role == TurnRole::User => {
            return json_error(
                StatusCode::CONFLICT,
                "turn_in_progress",
                &t(lang, "agent-embed-message-waiting"),
            );
        }
        Ok(_) => {}
        Err(err) => return internal(err),
    }
    let user_turn_id = uuid::Uuid::new_v4().to_string();
    if let Err(err) = chat::create_user_turn(&state.db, session_id, &user_turn_id, text).await {
        return internal(err);
    }
    json_ok(
        StatusCode::ACCEPTED,
        json!({ "turn_id": null, "user_turn_id": user_turn_id, "placement": "queued" }),
    )
}

fn not_waiting(lang: Lang) -> Response {
    json_error(
        StatusCode::CONFLICT,
        "not_suspended",
        &t(lang, "agent-embed-not-waiting"),
    )
}

/// A refused visitor answer, in the visitor's language where they can act on
/// it; the rest names the same codes as the staff route.
fn visitor_resume_refused(err: AgentResumeError, lang: Lang) -> Response {
    match err {
        AgentResumeError::StaffOnly { .. } => json_error(
            StatusCode::FORBIDDEN,
            "decision_for_staff",
            &t(lang, "agent-embed-decision-for-staff"),
        ),
        AgentResumeError::Refused(ResumeRefused::NotSuspended)
        | AgentResumeError::Refused(ResumeRefused::StaleRequest { .. }) => not_waiting(lang),
        other => super::agent_errors::resume_error(other),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeBody {
    pub request_id: String,
    pub decision: chat::DecisionKind,
    #[serde(default)]
    pub value: Option<Value>,
}

/// POST /api/v0/embed/resume — the visitor answers what the conversation
/// waits for: a secure input (a verification code), never an approval,
/// which is staff's (`POST /api/v0/agents/{id}/conversations/…/resume`).
/// 202 once the turn runs again; the rest arrives on
/// `GET /api/v0/embed/events`.
///
/// A `value` goes to the tool that asked for it and nowhere else: not into
/// the transcript, the model's context, a log line or the audit trail.
pub async fn resume(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let v = or_return!(visitor(&state, &req).await);
    let lang = Lang::from_request(req.headers());
    let ip = state.client_ip(&req);
    let body: ResumeBody = or_return!(
        super::read_json_capped(req.into_body(), "the resume body", MAX_BODY_BYTES).await
    );
    let decision = match super::chat::json_api::decision_from(body.decision, body.value) {
        Ok(decision) => decision,
        Err(msg) => return bad_request(msg),
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
    let Some(runner) = state.agent_turns.runner() else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "agent_runtime_unavailable",
            "this gateway cannot run agent conversations yet — try again after the gateway has \
             been updated",
        );
    };
    let session_id = v.session.session_id.clone();
    let busy = || {
        json_error(
            StatusCode::CONFLICT,
            "turn_in_progress",
            "the assistant is busy with this conversation right now — wait for it on \
             GET /api/v0/embed/events, then answer again",
        )
    };
    let turn_id = match chat::suspended_turn_in_session(&state.db, &session_id).await {
        Ok(Some(turn)) => turn,
        Ok(None) if state.agent_turns.is_running(&session_id) => return busy(),
        Ok(None) => return not_waiting(lang),
        Err(err) => return internal(err),
    };
    let Some(hold) = state.agent_turns.claim(&session_id, &turn_id) else {
        return busy();
    };
    let claimed = match claim_resume(
        &state,
        AgentResume {
            agent_id: &v.session.principal_id,
            session_id: &session_id,
            turn_id: &turn_id,
            request_id: Some(&body.request_id),
            decision,
            by: ResumedBy::Participant,
        },
    )
    .await
    {
        Ok(claimed) => claimed,
        Err(err) => return visitor_resume_refused(err, lang),
    };
    embed_rt::spawn_guarded(state, runner, hold, TurnWork::Resume(claimed));
    json_ok(StatusCode::ACCEPTED, json!({ "turn_id": turn_id }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityBody {
    pub token: String,
}

/// Longest identity token accepted, in bytes. A signed claim set for one
/// visitor is a few hundred; this only stops abuse.
const MAX_IDENTITY_TOKEN: usize = 8 * 1024;

/// POST /api/v0/embed/identity — the embedding website vouches for its
/// visitor with a token it signed (`docs/embed.md` "Signed-in visitors").
/// The agent's `host_jwt` verifier checks it and writes the claims it maps
/// into slots as `host`. `200 {slots}` names what was set, never a value.
pub async fn identity(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    use aiplane_runtime::agents::verifier::host_jwt::{self, IdentityError};
    let v = or_return!(visitor(&state, &req).await);
    let body: IdentityBody = or_return!(
        super::read_json_capped(req.into_body(), "the identity body", MAX_BODY_BYTES).await
    );
    if body.token.len() > MAX_IDENTITY_TOKEN {
        return bad_request(format!(
            "the identity token is longer than {MAX_IDENTITY_TOKEN} bytes — sign only the claims \
             the agent maps"
        ));
    }
    let accepted = host_jwt::accept(
        &state,
        &v.session.principal_id,
        &v.session.session_id,
        &v.live.spec,
        &body.token,
        Timestamp::now(),
    )
    .await;
    match accepted {
        Ok(slots) => json_ok(StatusCode::OK, json!({ "slots": slots })),
        Err(err) => {
            let status = match &err {
                IdentityError::NotConfigured => StatusCode::UNPROCESSABLE_ENTITY,
                IdentityError::Invalid(_) => StatusCode::UNAUTHORIZED,
                IdentityError::Replayed => StatusCode::CONFLICT,
                IdentityError::KeysUnavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
                IdentityError::Storage(_) => return internal(err),
            };
            json_error(status, err.code(), &err.to_string())
        }
    }
}

/// GET /api/v0/embed/events — the conversation's `chat_json` events, read
/// by the widget with `fetch` streaming (an `EventSource` cannot send the
/// visitor token).
///
/// A `snapshot` first. Then, if a turn is running, nothing but keep-alive
/// comments until it is terminal, and its answer as one `turn_delta`
/// (`full: true`) plus `turn_finalized`; or, when it paused for a decision,
/// `suspended` with what it waits for. Without a running turn, `suspended`
/// when the conversation waits for a decision, otherwise `idle`.
pub async fn events(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let v = or_return!(visitor(&state, &req).await);
    let session_id = v.session.session_id;
    let lang = Lang::from_request(req.headers());
    let releases = state.agent_turns.releases(&session_id);
    let turns = or_return!(visitor_turns(&state, &session_id, lang).await);
    let live = live_turn_id(&turns);
    let waiting = suspended_frame(&turns);
    let queued = queued_message(&turns);
    let (tx, rx) = rama::futures::channel::mpsc::unbounded();
    let snapshot = ChatEvent::Snapshot {
        live_turn_id: live.clone(),
        turns,
        waiting_turn_ids: queued.into_iter().collect(),
    };
    let _ = tx.unbounded_send(Ok(sse_json(&snapshot)));
    match (live, waiting) {
        (None, Some(frame)) => {
            let _ = tx.unbounded_send(Ok(sse_json(&frame)));
        }
        (None, None) => {
            let _ = tx.unbounded_send(Ok(sse_json(&ChatEvent::Idle)));
        }
        (Some(turn_id), _) => {
            let state = state.clone();
            tokio::spawn(async move {
                tail_buffered(state, session_id, turn_id, lang, releases, tx).await
            });
        }
    }
    json_stream_response(rx)
}

fn live_turn_id(turns: &[TurnWithTools]) -> Option<String> {
    turns
        .iter()
        .rev()
        .find(|t| t.turn.status == TurnStatus::InProgress)
        .map(|t| t.turn.id.clone())
}

/// The `suspended` frame of the conversation's paused turn, from turns
/// already in the visitor's view (see [`visitor_turn`]).
fn suspended_frame(turns: &[TurnWithTools]) -> Option<ChatEvent> {
    turns.iter().rev().find_map(|t| {
        Some(ChatEvent::Suspended {
            turn_id: t.turn.id.clone(),
            suspension: t.suspension.clone()?,
        })
    })
}

async fn tail_buffered(
    state: Arc<RamaState>,
    session_id: String,
    turn_id: String,
    lang: Lang,
    releases: ReleaseWatch,
    tx: SseTx,
) {
    let mut wait = TurnWait::new(releases);
    loop {
        match wait.next(&tx).await {
            Waited::Reread => {}
            Waited::Expired => {
                let _ = tx.unbounded_send(Ok(sse_json(&ChatEvent::Idle)));
                return;
            }
            Waited::Gone => return,
        }
        match chat::get_turn(&state.db, &session_id, &turn_id).await {
            Ok(Some(t)) if t.status.is_terminal() && !state.agent_turns.is_running(&session_id) => {
                for event in final_events(&t, lang) {
                    let _ = tx.unbounded_send(Ok(sse_json(&event)));
                }
                return;
            }
            Ok(Some(t))
                if t.status == TurnStatus::Suspended
                    && !state.agent_turns.is_running(&session_id) =>
            {
                let frame = match chat::get_suspension(&state.db, &turn_id).await {
                    Ok(Some(s)) => ChatEvent::Suspended {
                        turn_id: turn_id.clone(),
                        suspension: s.view().for_participant(),
                    },
                    _ => ChatEvent::Idle,
                };
                let _ = tx.unbounded_send(Ok(sse_json(&frame)));
                return;
            }
            Ok(Some(_)) => {}
            Ok(_) => {
                let _ = tx.unbounded_send(Ok(sse_json(&ChatEvent::Idle)));
                return;
            }
            Err(err) => {
                tracing::warn!(error = %err, turn = %turn_id, "embed events: reading the turn");
                let _ = tx.unbounded_send(Ok(sse_json(&ChatEvent::Idle)));
                return;
            }
        }
    }
}

/// The whole answer of a terminal turn, as the visitor sees it.
fn final_events(turn: &chat::Turn, lang: Lang) -> Vec<ChatEvent> {
    let mut events = Vec::new();
    if turn.status != TurnStatus::Errored
        && let Some(content) = turn.content.as_deref().filter(|c| !c.is_empty())
    {
        events.push(ChatEvent::TurnDelta {
            turn_id: turn.id.clone(),
            text_delta: content.to_string(),
            full: true,
        });
    }
    events.push(ChatEvent::TurnFinalized {
        turn_id: turn.id.clone(),
        status: turn.status.as_str().to_string(),
        error_message: (turn.status == TurnStatus::Errored).then(|| visitor_error(lang)),
        model: None,
        duration_ms: turn
            .completed_at
            .map(|done| done.duration_since(turn.created_at).as_millis() as i64),
    });
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use session_core::db::{ToolCall, ToolCallStatus, Turn};

    fn assistant(status: TurnStatus) -> TurnWithTools {
        let now = Timestamp::now();
        TurnWithTools {
            turn: Turn {
                id: "t".into(),
                session_id: "s".into(),
                seq: 1,
                role: TurnRole::Assistant,
                user_content: None,
                model: Some("internal-pool".into()),
                content: Some("half an ans".into()),
                reasoning: Some("thinking about the customer id".into()),
                reasoning_elapsed_ms: Some(5),
                reasoning_started_at: Some(now),
                status,
                error_message: Some("upstream 502 from backend gpu0".into()),
                created_at: now,
                completed_at: None,
            },
            tool_calls: vec![ToolCall {
                id: "c".into(),
                turn_id: "t".into(),
                seq: 0,
                name: "erp_lookup".into(),
                arguments_json: "{}".into(),
                output_json: Some("{\"customer\":\"K-12345\"}".into()),
                status: ToolCallStatus::Completed,
                created_at: now,
                completed_at: Some(now),
            }],
            steers: Vec::new(),
            suspension: None,
        }
    }

    #[test]
    fn a_visitor_sees_neither_tools_reasoning_nor_the_model() {
        let t = visitor_turn(assistant(TurnStatus::Completed), Lang::En);
        assert!(t.tool_calls.is_empty());
        assert_eq!(t.turn.reasoning, None);
        assert_eq!(t.turn.reasoning_started_at, None);
        assert_eq!(t.turn.model, None);
        assert_eq!(t.turn.content.as_deref(), Some("half an ans"));
    }

    #[test]
    fn a_paused_turn_shows_the_visitor_what_it_waits_for_and_nothing_of_the_tool() {
        let view = chat::SuspensionView {
            request_id: "req-1".into(),
            kind: chat::SuspensionKind::Approval,
            message: None,
            tool_call_id: Some("c".into()),
            tool: Some("erp_refund".into()),
            options: vec![chat::DecisionKind::AllowOnce, chat::DecisionKind::Deny],
            expires_at: Timestamp::now(),
        };
        let mut paused = assistant(TurnStatus::Suspended);
        paused.suspension = Some(view.clone());
        let seen = visitor_turn(paused, Lang::En);
        let shown = seen.suspension.expect("the visitor sees the pause");
        assert_eq!(shown.request_id, "req-1");
        assert_eq!(shown.tool, None);
        assert_eq!(shown.tool_call_id, None);
        assert!(shown.options.is_empty(), "an approval is staff's");
        assert_eq!(seen.turn.content, None, "no half answer");

        let mut done = assistant(TurnStatus::Completed);
        done.suspension = Some(view);
        assert_eq!(visitor_turn(done, Lang::En).suspension, None);
    }

    #[test]
    fn an_unfinished_answer_is_withheld_and_an_error_is_generic() {
        let running = visitor_turn(assistant(TurnStatus::InProgress), Lang::En);
        assert_eq!(running.turn.content, None);
        let errored = visitor_turn(assistant(TurnStatus::Errored), Lang::En);
        assert_eq!(
            errored.turn.error_message.as_deref(),
            Some("Something went wrong. Please try again.")
        );
        let in_german = visitor_turn(assistant(TurnStatus::Errored), Lang::De);
        assert_eq!(
            in_german.turn.error_message.as_deref(),
            Some("Etwas ist schiefgelaufen. Bitte versuchen Sie es erneut.")
        );
    }

    #[test]
    fn a_finished_answer_is_one_full_delta_then_the_finalize() {
        let mut t = assistant(TurnStatus::Completed).turn;
        t.content = Some("Hello".into());
        t.completed_at = Some(t.created_at);
        let events = final_events(&t, Lang::En);
        assert_eq!(
            events[0],
            ChatEvent::TurnDelta {
                turn_id: "t".into(),
                text_delta: "Hello".into(),
                full: true
            }
        );
        assert!(matches!(
            &events[1],
            ChatEvent::TurnFinalized { status, error_message: None, model: None, .. }
                if status == "completed"
        ));

        t.status = TurnStatus::Errored;
        let events = final_events(&t, Lang::En);
        assert_eq!(
            events.len(),
            1,
            "an errored turn's partial text is not shown"
        );
    }

    #[test]
    fn only_a_bearer_header_carries_the_visitor_token() {
        let mut h = HeaderMap::new();
        assert_eq!(bearer(&h), None);
        h.insert(header::AUTHORIZATION, "Bearer gwv_x".parse().unwrap());
        assert_eq!(bearer(&h), Some("gwv_x"));
        h.insert(header::AUTHORIZATION, "gwv_x".parse().unwrap());
        assert_eq!(bearer(&h), None);
    }
}
