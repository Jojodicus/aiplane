// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! What the public agent endpoint (`/api/v0/embed/*`, `docs/agents.md` §5)
//! needs from the runtime: the visitor-session settings read from an agent's
//! live spec, and the seam through which it runs one turn.
//!
//! The endpoint owns everything about the visitor — key, origin, token, TTL,
//! which conversation — and opens the turn rows. Running the turn as the
//! agent's principal is [`AgentTurnRunner`]'s job, so the endpoint never
//! touches the loop, grants or gates.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use jiff::SignedDuration;
use serde_json::Value;

use session_core::db as chat;

use super::profile::{Role, RunOptions, RunProfile};
pub use super::run::OpenedTurn;
use super::run::drive_opened;
use crate::rama_server::state::RamaState;

/// A visitor session's idle TTL when the spec names no `publish.idle_ttl`.
pub const DEFAULT_IDLE_TTL: SignedDuration = SignedDuration::from_secs(30 * 60);

/// The absolute cap on one visitor session, however often it is used:
/// `sessionStorage` already ends it with the tab, and this ends a tab that
/// is never closed.
pub const MAX_VISITOR_SESSION: SignedDuration = SignedDuration::from_secs(24 * 60 * 60);

/// `publish.idle_ttl` of a spec, or [`DEFAULT_IDLE_TTL`]. A published spec
/// passed validation, so an unparsable value only comes from a hand-edited
/// row; it falls back rather than locking every visitor out.
pub fn idle_ttl(spec: &Value) -> SignedDuration {
    spec.pointer("/publish/idle_ttl")
        .and_then(Value::as_str)
        .and_then(super::spec::parse_duration)
        .unwrap_or(DEFAULT_IDLE_TTL)
}

/// Whether the spec lets a browser on `origin` use the agent: always when it
/// sets no `publish.origins`, otherwise only an origin listed there. The
/// embed key's own list applies on top; an origin must pass both.
pub fn spec_allows_origin(spec: &Value, origin: &str) -> bool {
    match spec.pointer("/publish/origins").and_then(Value::as_array) {
        None => true,
        Some(list) => list.iter().any(|o| o.as_str() == Some(origin)),
    }
}

/// Runs one opened turn of an agent conversation as the agent's principal.
///
/// Contract:
/// - The turn runs as `agent_id` with exactly its grants, on `version` (the
///   version the conversation started on), never as a person.
/// - Output is buffered (`OutputPolicy::Buffered`): the endpoint shows a
///   visitor an assistant answer only once its turn is terminal, so a runner
///   may write partial content as it likes.
/// - When `run` returns, `turn_id` is terminal. The endpoint errors a turn
///   left `in_progress`, so a runner that crashes cannot wedge the
///   conversation.
#[async_trait::async_trait]
pub trait AgentTurnRunner: Send + Sync {
    async fn run(&self, state: Arc<RamaState>, turn: OpenedTurn);
}

/// The production runner: the agent entry point of `agents::run`.
#[derive(Debug, Clone, Copy, Default)]
pub struct LiveAgentRunner;

#[async_trait::async_trait]
impl AgentTurnRunner for LiveAgentRunner {
    async fn run(&self, state: Arc<RamaState>, turn: OpenedTurn) {
        let options = RunOptions::default();
        let ran = match RunProfile::load_version(
            &state,
            &turn.agent_id,
            Some(turn.version),
            Role::Main,
            &options,
        )
        .await
        {
            Ok(profile) => drive_opened(&state, &profile, &turn).await.map(|_| ()),
            Err(err) => Err(err),
        };
        if let Err(err) = ran {
            tracing::warn!(error = %err, turn = %turn.turn_id, "visitor turn could not run");
            if let Err(db) = chat::finalize_turn(
                &state.db,
                &turn.turn_id,
                chat::TurnStatus::Errored,
                Some(&err.to_string()),
            )
            .await
            {
                tracing::warn!(error = %db, turn = %turn.turn_id, "recording a failed visitor turn");
            }
        }
    }
}

/// The runner, if this build has one, and the conversations with a turn
/// running right now.
#[derive(Clone, Default)]
pub struct AgentTurns {
    runner: Option<Arc<dyn AgentTurnRunner>>,
    running: Arc<Mutex<HashSet<String>>>,
}

impl AgentTurns {
    pub fn new(runner: Arc<dyn AgentTurnRunner>) -> Self {
        Self {
            runner: Some(runner),
            running: Arc::default(),
        }
    }

    pub fn runner(&self) -> Option<Arc<dyn AgentTurnRunner>> {
        self.runner.clone()
    }

    /// Claim `session_id` for one turn. `None` when a turn already holds it;
    /// the claim ends when the returned guard drops.
    pub fn claim(&self, session_id: &str) -> Option<TurnClaim> {
        let mut running = self.running.lock().unwrap_or_else(|p| p.into_inner());
        if !running.insert(session_id.to_string()) {
            return None;
        }
        Some(TurnClaim {
            running: self.running.clone(),
            session_id: session_id.to_string(),
        })
    }
}

/// See [`AgentTurns::claim`].
pub struct TurnClaim {
    running: Arc<Mutex<HashSet<String>>>,
    session_id: String,
}

impl Drop for TurnClaim {
    fn drop(&mut self) {
        let mut running = self.running.lock().unwrap_or_else(|p| p.into_inner());
        running.remove(&self.session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_idle_ttl_comes_from_the_publish_settings_with_a_30_minute_default() {
        assert_eq!(
            idle_ttl(&json!({ "publish": { "idle_ttl": "45m" } })),
            SignedDuration::from_secs(45 * 60)
        );
        assert_eq!(idle_ttl(&json!({ "publish": {} })), DEFAULT_IDLE_TTL);
        assert_eq!(idle_ttl(&json!({})), DEFAULT_IDLE_TTL);
        assert_eq!(
            idle_ttl(&json!({ "publish": { "idle_ttl": "soon" } })),
            DEFAULT_IDLE_TTL
        );
        assert_eq!(DEFAULT_IDLE_TTL, SignedDuration::from_secs(1800));
    }

    #[test]
    fn the_specs_origins_narrow_only_when_it_lists_some() {
        let open = json!({ "publish": {} });
        assert!(spec_allows_origin(&open, "https://any.example"));
        let listed = json!({ "publish": { "origins": ["https://a.example"] } });
        assert!(spec_allows_origin(&listed, "https://a.example"));
        assert!(!spec_allows_origin(&listed, "https://b.example"));
        let empty = json!({ "publish": { "origins": [] } });
        assert!(!spec_allows_origin(&empty, "https://a.example"));
    }

    #[test]
    fn a_conversation_holds_one_claim_at_a_time() {
        let turns = AgentTurns::default();
        assert!(turns.runner().is_none());
        let first = turns.claim("s1").expect("free");
        assert!(turns.claim("s1").is_none(), "already running");
        assert!(turns.claim("s2").is_some(), "other conversations are free");
        drop(first);
        assert!(turns.claim("s1").is_some(), "released on drop");
    }
}
