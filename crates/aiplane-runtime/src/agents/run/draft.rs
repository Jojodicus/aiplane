// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The internal test chat's half of the run path (#90): one turn of an
//! agent's **draft**, and what a manager may see of it that a visitor never
//! does.
//!
//! The draft is passed explicitly as [`SpecSource::Draft`]; nothing about
//! "live" is overridden anywhere, so no other path can reach a draft. The turn
//! then goes through the same `open_session` and `drive_opened` as a
//! visitor's, so gates, slots, tools, binds and budgets behave as they will
//! once published. The conversation is recorded as version [`DRAFT_VERSION`],
//! which no published version has: a visitor session can never continue it,
//! and a test session can never be continued as one.

use std::sync::Arc;

use aiplane_core::server::db::{DbError, agent_audit, agents as agents_db};
use jiff::Timestamp;
use serde::Serialize;
use serde_json::Value;
use session_core::db as chat;

use super::{AgentReply, AgentTurn, OpenedTurn, drive_opened};
use crate::agents::gate::{GateInput, GateStatus, RouteGates, Unmet};
use crate::agents::profile::{AgentRunError, Role, RunOptions, RunProfile, SpecSource};
use crate::agents::state::{AgentState, Provenance, SlotState, StateSchema};
use crate::rama_server::state::RamaState;
use crate::server::headless::{OpenParams, Owner, open_session};

pub use agents_db::DRAFT_VERSION;

/// Run one visitor message against `draft` as the agent's principal.
pub async fn run_draft_turn(
    state: &Arc<RamaState>,
    turn: AgentTurn<'_>,
    draft: &Value,
    options: RunOptions,
) -> Result<AgentReply, AgentRunError> {
    if let Some(session) = turn.session_id {
        let continues = chat::get_principal_session(&state.db, turn.agent_id, session)
            .await
            .map_err(DbError::from)?
            .is_some_and(|run| run.agent_version == Some(DRAFT_VERSION));
        if !continues {
            let agent = aiplane_core::server::db::system_principals::get(&state.db, turn.agent_id)
                .await?
                .map_or_else(|| turn.agent_id.to_string(), |p| p.name);
            return Err(AgentRunError::UnknownSession {
                agent,
                session: session.to_string(),
            });
        }
        super::refuse_if_waiting(state, session).await?;
    }
    let profile = RunProfile::load_from(
        state,
        turn.agent_id,
        SpecSource::Draft(draft.clone()),
        Role::Main,
        &options,
    )
    .await?;
    let (session_id, turn_id) = open_session(
        &state.db,
        OpenParams {
            owner: Owner::Run {
                principal_id: &profile.principal.id,
                parent_turn_id: None,
                agent_version: Some(DRAFT_VERSION),
            },
            title: &profile.principal.name,
            prompt: turn.message,
            model: &profile.model,
            existing_session: turn.session_id.map(str::to_string),
        },
    )
    .await?;
    let opened = OpenedTurn {
        agent_id: profile.principal.id.clone(),
        version: DRAFT_VERSION,
        session_id,
        turn_id,
        visitor_id: None,
        lang: options.lang,
    };
    drive_opened(state, &profile, &opened).await
}

#[derive(Debug, Clone, Serialize)]
pub struct SlotDebug {
    pub slot: String,
    pub status: &'static str,
    /// Why the slot is `invalid`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The stored value, whoever wrote it: a manager may see what a verifier
    /// stored, the model may not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_at: Option<String>,
    pub set_by: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RouteDebug {
    pub route: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub open: bool,
    pub missing: Vec<Unmet>,
}

/// What the run left behind, for the manager testing the draft.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DraftDebug {
    pub slots: Vec<SlotDebug>,
    pub routes: Vec<RouteDebug>,
    /// Each `forward_request` decision of this turn: every route's gate and
    /// the route picked, if any.
    pub routing: Vec<Value>,
    /// Each sub-agent dispatch of this turn, with its outcome once finished.
    pub sub_agents: Vec<Value>,
    /// Each tool call of this turn and the grant decision on it.
    pub tool_calls: Vec<Value>,
}

/// The state after the turn, and the audit rows the turn wrote (`since`
/// bounds the conversation's earlier turns out).
pub async fn collect_debug(
    state: &RamaState,
    agent_id: &str,
    draft: &Value,
    session_id: &str,
    since: Timestamp,
    options: &RunOptions,
) -> Result<DraftDebug, DbError> {
    let (Ok(schema), Ok(gates)) = (StateSchema::from_spec(draft), RouteGates::from_spec(draft))
    else {
        return Ok(DraftDebug::default());
    };
    let stored = AgentState::load(&state.db, &schema, session_id).await?;
    let slots = schema
        .slots()
        .map(|def| slot_debug(def.name.clone(), stored.get(&def.name), &def.set_by))
        .collect();
    let routes = gates
        .statuses(GateInput {
            schema: &schema,
            state: &stored,
            now: (options.now)(),
        })
        .into_iter()
        .map(|(route, status)| {
            let (open, missing) = match status {
                GateStatus::Open => (true, Vec::new()),
                GateStatus::Closed { missing } => (false, missing),
            };
            RouteDebug {
                description: draft
                    .pointer(&format!("/routes/{route}/description"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                route,
                open,
                missing,
            }
        })
        .collect();

    let mut debug = DraftDebug {
        slots,
        routes,
        ..DraftDebug::default()
    };
    let mut events = agent_audit::for_principal(&state.db, agent_id).await?;
    events.reverse();
    for event in events.into_iter().filter(|e| {
        e.created_at >= since
            && e.chain
                .as_ref()
                .and_then(|c| c.get("root_session"))
                .and_then(Value::as_str)
                == Some(session_id)
    }) {
        match event.kind.as_str() {
            "route_decision" => debug.routing.push(event.detail),
            "sub_agent_dispatched" => debug.sub_agents.push(event.detail),
            "sub_agent_finished" => {
                let turn = event.detail.get("turn_id").cloned();
                match debug
                    .sub_agents
                    .iter_mut()
                    .find(|d| d.get("turn_id") == turn.as_ref())
                {
                    Some(dispatched) => *dispatched = event.detail,
                    None => debug.sub_agents.push(event.detail),
                }
            }
            "tool_call" => debug.tool_calls.push(event.detail),
            _ => {}
        }
    }
    Ok(debug)
}

fn slot_debug(slot: String, state: &SlotState, set_by: &[Provenance]) -> SlotDebug {
    let set_by = set_by.iter().map(ToString::to_string).collect();
    let (status, reason, entry) = match state {
        SlotState::Missing => ("missing", None, None),
        SlotState::Set(entry) => ("set", None, Some(entry)),
        SlotState::Invalid { entry, reason } => ("invalid", Some(reason.clone()), Some(entry)),
    };
    SlotDebug {
        slot,
        status,
        reason,
        value: entry.map(|e| e.value.clone()),
        provenance: entry.map(|e| e.provenance.to_string()),
        set_at: entry.map(|e| e.set_at.to_string()),
        set_by,
    }
}
