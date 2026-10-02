// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Run one turn of an agent conversation as the agent's principal, on its
//! live version. The Rust entry point the internal test chat (#90) and the
//! public endpoint (#91) put HTTP in front of.

use std::sync::Arc;

use aiplane_core::server::db::DbError;
use aiplane_core::server::run_chain::{Frame, RunChain};
use session_core::db as chat;

use super::profile::{AgentRunError, Role, RunOptions, RunProfile};
use crate::rama_server::state::RamaState;
use crate::server::headless::{OpenParams, Owner, drive, open_session};

/// One visitor message to an agent.
pub struct AgentTurn<'a> {
    /// The agent's id (its principal's).
    pub agent_id: &'a str,
    /// Continue this conversation; `None` starts one.
    pub session_id: Option<&'a str>,
    pub message: &'a str,
    /// The visitor session behind the conversation, for the call chain.
    pub visitor_id: Option<&'a str>,
}

/// How the turn ended.
#[derive(Debug, Clone)]
pub struct AgentReply {
    pub session_id: String,
    pub turn_id: String,
    pub status: chat::TurnStatus,
    pub answer: Option<String>,
    pub error: Option<String>,
}

pub async fn run_turn(
    state: &Arc<RamaState>,
    turn: AgentTurn<'_>,
) -> Result<AgentReply, AgentRunError> {
    run_turn_with(state, turn, RunOptions::default()).await
}

/// [`run_turn`] with its seams supplied.
pub async fn run_turn_with(
    state: &Arc<RamaState>,
    turn: AgentTurn<'_>,
    options: RunOptions,
) -> Result<AgentReply, AgentRunError> {
    let profile = RunProfile::load(state, turn.agent_id, Role::Main, &options).await?;
    if let Some(session) = turn.session_id
        && chat::get_principal_session(&state.db, &profile.principal.id, session)
            .await
            .map_err(DbError::from)?
            .is_none()
    {
        return Err(AgentRunError::UnknownSession {
            agent: profile.principal.name.clone(),
            session: session.to_string(),
        });
    }
    let (session_id, turn_id) = open_session(
        &state.db,
        OpenParams {
            owner: Owner::Run {
                principal_id: &profile.principal.id,
                parent_turn_id: None,
                agent_version: Some(profile.version),
            },
            title: &profile.principal.name,
            prompt: turn.message,
            model: &profile.model,
            existing_session: turn.session_id.map(str::to_string),
        },
    )
    .await?;
    let chain = Arc::new(RunChain::root(
        &session_id,
        turn.visitor_id.map(str::to_string),
        Frame::for_principal(&profile.principal, Some(profile.version)),
    ));
    drive(state, profile.drive_params(&session_id, &turn_id, chain)).await;
    let done = chat::get_turn(&state.db, &session_id, &turn_id)
        .await
        .map_err(DbError::from)?;
    Ok(AgentReply {
        status: done
            .as_ref()
            .map_or(chat::TurnStatus::Errored, |t| t.status),
        answer: done.as_ref().and_then(|t| t.content.clone()),
        error: done.and_then(|t| t.error_message),
        session_id,
        turn_id,
    })
}

pub mod draft;

#[cfg(test)]
mod tests;
