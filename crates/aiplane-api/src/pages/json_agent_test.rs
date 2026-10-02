// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! `/api/v0/agents/{id}/test-turn` — the internal test chat (`docs/agents.md`
//! §3, #90): a manager sends a message to the agent's **draft** and gets the
//! answer plus what a visitor never sees — slot values and provenance, each
//! route's gate, the routing decision, sub-agent calls and tool-call
//! decisions.
//!
//! The turn is the real run path ([`run_draft_turn`]): the draft's grants,
//! gates, binds and budgets apply, and its tools really run as the agent's
//! principal, so it needs a `write` share. Omitting `session_id` starts a
//! conversation; passing one continues it.
//!
//! `/api/v0/agents/{id}/conversations/{session}/turns/{turn}/resume` is the
//! staff side of a suspended agent run: an approval (or a human answer) for
//! any of the agent's conversations, and any decision in a test
//! conversation, where the manager is the visitor too.

use std::sync::Arc;

use jiff::Timestamp;
use rama::http::service::web::extract::State;
use rama::http::{Request, Response, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

use super::json_agents::{agent_at, parse_spec};
use super::json_principals::require_agent_manager;
use super::{bad_request, internal, json_error, json_ok, raw_path_segment};
use aiplane_core::server::db::agents::Access;
use aiplane_runtime::agents::profile::{AgentRunError, RunOptions};
use aiplane_runtime::agents::resume::{
    AgentResume, AgentResumeError, ResumedBy, claim, run_claimed,
};
use aiplane_runtime::agents::run::draft::{DRAFT_VERSION, collect_debug, run_draft_turn};
use aiplane_runtime::agents::run::{AgentReply, AgentTurn};
use aiplane_runtime::rama_server::state::RamaState;
use aiplane_runtime::suspend::ResumeRefused;
use session_core::db as chat;
use session_core::i18n::Lang;

macro_rules! or_return {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(resp) => return resp,
        }
    };
}

#[derive(Deserialize)]
pub struct TestTurnBody {
    pub message: String,
    #[serde(default)]
    pub session_id: Option<String>,
}

fn run_error(err: AgentRunError) -> Response {
    let (status, code) = match &err {
        AgentRunError::Unavailable(_) => (StatusCode::NOT_FOUND, "agent_unavailable"),
        AgentRunError::UnknownSession { .. } => (StatusCode::NOT_FOUND, "unknown_session"),
        AgentRunError::DecisionPending { .. } => (StatusCode::CONFLICT, "decision_pending"),
        AgentRunError::NotLive(_)
        | AgentRunError::BadSpec { .. }
        | AgentRunError::MissingVersion { .. } => {
            (StatusCode::UNPROCESSABLE_ENTITY, "agent_not_runnable")
        }
        AgentRunError::NoModel { .. } => (StatusCode::SERVICE_UNAVAILABLE, "agent_no_model"),
        AgentRunError::Db(_) => return internal(err),
    };
    json_error(status, code, &err.to_string())
}

/// A refused resume, in the `/api/v0` envelope. Shared with the visitor's
/// route, so both name a refusal the same way.
pub(crate) fn resume_error(err: AgentResumeError) -> Response {
    let (status, code) = match &err {
        AgentResumeError::Refused(ResumeRefused::NotSuspended)
        | AgentResumeError::Refused(ResumeRefused::StaleRequest { .. }) => {
            (StatusCode::CONFLICT, "not_suspended")
        }
        AgentResumeError::Refused(ResumeRefused::NotOffered { .. }) => {
            (StatusCode::BAD_REQUEST, "decision_not_offered")
        }
        AgentResumeError::StaffOnly { .. } => (StatusCode::FORBIDDEN, "decision_for_staff"),
        AgentResumeError::ParticipantOnly { .. } => (StatusCode::FORBIDDEN, "decision_for_visitor"),
        AgentResumeError::Refused(ResumeRefused::Storage(_)) | AgentResumeError::Db(_) => {
            return internal(err);
        }
    };
    json_error(status, code, &err.to_string())
}

fn reply_json(reply: &AgentReply) -> Value {
    json!({
        "session_id": reply.session_id,
        "turn_id": reply.turn_id,
        "status": reply.status,
        "answer": reply.answer,
        "error": reply.error,
        "suspension": reply.suspension,
    })
}

/// POST /api/v0/agents/{id}/test-turn
pub async fn test_turn(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let user = or_return!(require_agent_manager(&state, &req).await);
    let (agent, _) = or_return!(agent_at(&state, &req, &user, 1, Access::Write).await);
    let body: TestTurnBody =
        or_return!(super::read_json(req.into_body(), "the test message").await);
    if body.message.trim().is_empty() {
        return bad_request("a test turn needs a non-empty `message`");
    }
    let id = agent.principal.id;
    let draft = parse_spec(&agent.draft_spec);
    let options = RunOptions::default();
    let started = Timestamp::now();
    let reply = match run_draft_turn(
        &state,
        AgentTurn {
            agent_id: &id,
            session_id: body.session_id.as_deref(),
            message: &body.message,
            visitor_id: None,
        },
        &draft,
        options.clone(),
    )
    .await
    {
        Ok(reply) => reply,
        Err(err) => return run_error(err),
    };
    let debug = match collect_debug(&state, &id, &draft, &reply.session_id, started, &options).await
    {
        Ok(debug) => debug,
        Err(err) => return internal(err),
    };
    let mut out = reply_json(&reply);
    out["draft_version"] = json!(DRAFT_VERSION);
    out["debug"] = json!(debug);
    json_ok(StatusCode::OK, out)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaffResumeBody {
    pub decision: chat::DecisionKind,
    #[serde(default)]
    pub value: Option<Value>,
    /// The `request_id` of the `suspended` frame being answered. Optional;
    /// when given, an answer to an earlier pause is refused.
    #[serde(default)]
    pub request_id: Option<String>,
}

/// POST /api/v0/agents/{id}/conversations/{session}/turns/{turn}/resume —
/// answer what a suspended agent turn waits for, as staff, and continue it.
///
/// `write` share (or admin). Synchronous like the test chat: the answer is
/// the resumed turn's reply, which may be another `suspended` one. The
/// visitor of the conversation sees the turn running meanwhile and gets the
/// answer on their own stream.
pub async fn resume_turn(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let user = or_return!(require_agent_manager(&state, &req).await);
    let (agent, _) = or_return!(agent_at(&state, &req, &user, 5, Access::Write).await);
    let (Some(session_id), Some(turn_id)) = (raw_path_segment(&req, 3), raw_path_segment(&req, 1))
    else {
        return bad_request("the URL is missing the conversation or the turn id");
    };
    let lang = Lang::from_request(req.headers());
    let body: StaffResumeBody =
        or_return!(super::read_json(req.into_body(), "the resume body").await);
    let request_id = body.request_id.clone();
    let decision = match super::chat::json_api::decision_from(body.decision, body.value) {
        Ok(decision) => decision,
        Err(msg) => return bad_request(msg),
    };
    let Some(_hold) = state.agent_turns.claim(&session_id) else {
        return json_error(
            StatusCode::CONFLICT,
            "turn_in_progress",
            "this conversation is running a turn right now — answer again once it has finished",
        );
    };
    let claimed = match claim(
        &state,
        AgentResume {
            agent_id: &agent.principal.id,
            session_id: &session_id,
            turn_id: &turn_id,
            request_id: request_id.as_deref(),
            decision,
            by: ResumedBy::Staff {
                user_id: user.id.clone(),
            },
        },
    )
    .await
    {
        Ok(claimed) => claimed,
        Err(err) => return resume_error(err),
    };
    match run_claimed(&state, claimed, RunOptions::default(), lang).await {
        Ok(reply) => json_ok(StatusCode::OK, reply_json(&reply)),
        Err(err) => run_error(err),
    }
}
