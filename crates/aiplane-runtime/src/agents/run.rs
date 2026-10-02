// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Run one turn of an agent conversation as the agent's principal, on its
//! live version. The Rust entry point the internal test chat (#90) and the
//! public endpoint (#91) put HTTP in front of.

use std::sync::Arc;

use aiplane_core::server::db::{DbError, system_principals as sp};
use aiplane_core::server::run_chain::{Frame, RunChain};
use session_core::db as chat;
use session_core::i18n::Lang;

use super::output_filter::{Delivery, guard_answer};
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
///
/// A new conversation runs the live version and records it in
/// `chat_sessions.agent_version`; a continued one keeps running the version
/// it started on, so publishing or rolling back never changes an open
/// conversation mid-way.
pub async fn run_turn_with(
    state: &Arc<RamaState>,
    turn: AgentTurn<'_>,
    options: RunOptions,
) -> Result<AgentReply, AgentRunError> {
    let pinned = match turn.session_id {
        None => None,
        Some(session) => {
            let Some(run) = chat::get_principal_session(&state.db, turn.agent_id, session)
                .await
                .map_err(DbError::from)?
            else {
                let agent = sp::get(&state.db, turn.agent_id)
                    .await?
                    .map_or_else(|| turn.agent_id.to_string(), |p| p.name);
                return Err(AgentRunError::UnknownSession {
                    agent,
                    session: session.to_string(),
                });
            };
            run.agent_version
        }
    };
    let profile =
        RunProfile::load_version(state, turn.agent_id, pinned, Role::Main, &options).await?;
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
    let opened = OpenedTurn {
        agent_id: profile.principal.id.clone(),
        version: profile.version,
        session_id,
        turn_id,
        visitor_id: turn.visitor_id.map(str::to_string),
        lang: options.lang,
    };
    drive_opened(state, &profile, &opened).await
}

/// A turn whose rows already exist: the visitor's user turn and an
/// `in_progress` assistant turn `turn_id` in the principal-owned conversation
/// `session_id`. The public endpoint opens these itself, because it must
/// answer before the turn has run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedTurn {
    pub agent_id: String,
    /// The version the conversation runs on.
    pub version: i64,
    pub session_id: String,
    pub turn_id: String,
    pub visitor_id: Option<String>,
    /// The language of text the gateway itself puts in the answer.
    pub lang: Lang,
}

/// Drive an opened turn of `profile` to a terminal status and read it back.
pub async fn drive_opened(
    state: &Arc<RamaState>,
    profile: &RunProfile,
    turn: &OpenedTurn,
) -> Result<AgentReply, AgentRunError> {
    let chain = Arc::new(RunChain::root(
        &turn.session_id,
        turn.visitor_id.clone(),
        Frame::for_principal(&profile.principal, Some(profile.version)),
    ));
    drive(
        state,
        profile.drive_params(&turn.session_id, &turn.turn_id, chain.clone()),
    )
    .await;
    let done = chat::get_turn(&state.db, &turn.session_id, &turn.turn_id)
        .await
        .map_err(DbError::from)?;
    let mut answer = done.as_ref().and_then(|t| t.content.clone());
    if let Some(filter) = &profile.output_filter
        && let Some(text) = answer.take()
    {
        let at = Delivery {
            principal_id: &profile.principal.id,
            session_id: &turn.session_id,
            turn_id: &turn.turn_id,
            chain: &chain,
            lang: turn.lang,
        };
        answer = Some(
            guard_answer(
                state,
                filter,
                profile.run.state_schema().map(|s| &**s),
                at,
                text,
            )
            .await,
        );
    }
    Ok(AgentReply {
        status: done
            .as_ref()
            .map_or(chat::TurnStatus::Errored, |t| t.status),
        answer,
        error: done.and_then(|t| t.error_message),
        session_id: turn.session_id.clone(),
        turn_id: turn.turn_id.clone(),
    })
}

pub mod draft;

#[cfg(test)]
mod tests;
