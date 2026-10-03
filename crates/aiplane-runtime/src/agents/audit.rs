// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The runtime's door to the activity log (`aiplane_agents::db::agent_audit`,
//! `docs/agents.md` → "What #111 built"). Every run event goes through
//! [`record_event`]: one event, written synchronously in its own write
//! transaction before the run moves on, so the log's order is the run's order
//! and nothing waits in memory to be lost.
//!
//! **Bound.** A write may take at most [`WRITE_BOUND`]: the lock wait (SQLite
//! serialises writers) and the write itself. Past it the event counts as not
//! written.
//!
//! **Fail closed.** An event of an agent run that could not be written marks
//! the run ([`AgentRun::mark_log_failed`]); the driver stops it at the next
//! step — before the next model round, before tools run, and when the turn
//! would end — and errors the turn, so nothing happens in a run that its log
//! does not show. Outside a run (a management change is written on the
//! change's own transaction instead; a refused visitor; a sweep) the failure
//! is logged at `error` with the event's correlation ids.

use std::time::Duration;

use aiplane_agents::db::agent_audit::{self, AuditKind, Correlation, NewEvent};
use aiplane_core::server::db::Pool;
use aiplane_core::server::run_chain::RunChain;
use serde_json::{Value, json};

use crate::agent_run::AgentRun;
use crate::server::tools::ToolContext;

/// The longest a run waits for one event to be written.
pub const WRITE_BOUND: Duration = Duration::from_secs(5);

/// Why an event did not reach the log.
#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error(
        "writing to the agent activity log took longer than {}s; the database is busy or the \
         disk is slow — check the server's disk and database lock contention",
        WRITE_BOUND.as_secs()
    )]
    TimedOut,
    #[error("writing to the agent activity log failed: {0}")]
    Db(#[from] aiplane_core::server::db::DbError),
}

/// What a turn stopped by [`LogError`] tells its participant: the run is
/// stopped rather than continued unrecorded.
pub const LOG_UNAVAILABLE: &str = "The agent's activity log could not be written, so the run was \
                                   stopped before doing anything it could not record. Try again \
                                   shortly; if it keeps happening, the gateway's database needs \
                                   attention.";

/// Append one event, bounded by [`WRITE_BOUND`]. A failure is logged here,
/// once, with the event's correlation ids.
pub async fn record_event(db: &Pool, event: NewEvent<'_>) -> Result<(), LogError> {
    let kind = event.kind.as_str();
    let principal = event.principal_id.to_string();
    let conversation = event
        .chain
        .map(|c| c.root_session.clone())
        .or_else(|| event.at.conversation_id.clone());
    let turn = event.at.turn_id.clone();
    let written = match tokio::time::timeout(WRITE_BOUND, agent_audit::append_now(db, event)).await
    {
        Ok(Ok(_)) => return Ok(()),
        Ok(Err(err)) => LogError::Db(err),
        Err(_) => LogError::TimedOut,
    };
    tracing::error!(
        error = %written,
        kind,
        principal = %principal,
        conversation = ?conversation,
        turn = ?turn,
        "an agent activity event could not be written"
    );
    Err(written)
}

/// One event of `principal_id` outside a tool context; `actor_id` names the
/// person who caused it, when one did. A failure is logged and marks `run`
/// when there is one.
pub async fn record(
    db: &Pool,
    kind: AuditKind,
    principal_id: &str,
    actor_id: Option<&str>,
    chain: Option<&RunChain>,
    detail: Value,
) {
    let _ = record_event(
        db,
        NewEvent::new(kind, principal_id, detail)
            .by(actor_id)
            .in_run(chain),
    )
    .await;
}

/// [`record_event`] for an event of `run`, which is stopped when the event
/// cannot be written.
pub(crate) async fn record_for_run(db: &Pool, run: Option<&AgentRun>, event: NewEvent<'_>) {
    if record_event(db, event).await.is_err()
        && let Some(run) = run
    {
        run.mark_log_failed();
    }
}

impl ToolContext {
    /// Where this call sits in its run: its conversation, turn, the round
    /// the driver is in, and the tool call.
    pub(crate) fn correlation(&self) -> Correlation {
        Correlation {
            session_id: self.session_id.clone(),
            turn_id: self.assistant_turn_id.clone(),
            round: self.agent.as_deref().map(AgentRun::round),
            call_id: self.call_id.clone(),
            conversation_id: None,
        }
    }

    /// One run event of this call's principal, with its call chain and
    /// correlation ids, and its conversation and turn added to `detail`
    /// unless it names them already. The run stops when it cannot be
    /// written.
    pub(crate) async fn audit(&self, kind: AuditKind, mut detail: Value) {
        if let Value::Object(map) = &mut detail {
            map.entry("session_id")
                .or_insert_with(|| json!(self.session_id));
            map.entry("turn_id")
                .or_insert_with(|| json!(self.assistant_turn_id));
        }
        self.audit_event(kind, None, detail).await;
    }

    /// [`Self::audit`] without touching `detail`, with how long the thing
    /// recorded took.
    pub(crate) async fn audit_event(
        &self,
        kind: AuditKind,
        duration_ms: Option<u64>,
        detail: Value,
    ) {
        let mut event = NewEvent::new(kind, self.principal.subject_id(), detail)
            .in_run(self.chain())
            .at(self.correlation());
        event.duration_ms = duration_ms;
        record_for_run(&self.db, self.agent.as_deref(), event).await;
    }
}
