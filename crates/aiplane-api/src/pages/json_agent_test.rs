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

use std::sync::Arc;

use jiff::Timestamp;
use rama::http::service::web::extract::State;
use rama::http::{Request, Response, StatusCode};
use serde::Deserialize;
use serde_json::json;

use super::json_agents::{agent_at, parse_spec};
use super::json_principals::require_agent_manager;
use super::{bad_request, internal, json_error, json_ok};
use aiplane_core::server::db::agents::Access;
use aiplane_runtime::agents::profile::{AgentRunError, RunOptions};
use aiplane_runtime::agents::run::AgentTurn;
use aiplane_runtime::agents::run::draft::{DRAFT_VERSION, collect_debug, run_draft_turn};
use aiplane_runtime::rama_server::state::RamaState;

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
        AgentRunError::NotLive(_) | AgentRunError::BadSpec { .. } => {
            (StatusCode::UNPROCESSABLE_ENTITY, "agent_not_runnable")
        }
        AgentRunError::NoModel { .. } => (StatusCode::SERVICE_UNAVAILABLE, "agent_no_model"),
        AgentRunError::Db(_) => return internal(err),
    };
    json_error(status, code, &err.to_string())
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
    json_ok(
        StatusCode::OK,
        json!({
            "session_id": reply.session_id,
            "turn_id": reply.turn_id,
            "status": reply.status,
            "answer": reply.answer,
            "error": reply.error,
            "draft_version": DRAFT_VERSION,
            "debug": debug,
        }),
    )
}
