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

use std::sync::Arc;
use std::time::{Duration, Instant};

use aiplane_agents::db::agent_audit::{self, AuditKind, Correlation, NewEvent};
use aiplane_core::server::db::Pool;
use aiplane_core::server::principal::SystemPrincipal;
use aiplane_core::server::run_chain::{Frame, RunChain};
use serde_json::{Value, json};

use crate::agent_run::AgentRun;
use crate::server::tools::ToolContext;

/// The longest a run waits for one event to be written.
pub const WRITE_BOUND: Duration = Duration::from_secs(5);

/// What the log leaves out of a run's tool calls, the same wherever a call
/// appears — its `tool_result` and every `llm_exchange` that carries it: the
/// arguments of a tool that declares them sensitive (`sensitive_args`), and
/// a value a resume decided, which the run's tool may have repeated.
#[derive(Clone, Copy, Default)]
pub(crate) struct Redaction<'a> {
    /// The value the turn was resumed with, withheld wherever it appears.
    pub decided: Option<&'a Value>,
}

impl Redaction<'_> {
    /// What stands in for the arguments of a tool that declares them
    /// sensitive.
    pub fn redacted_arguments() -> Value {
        json!({ "redacted": true })
    }

    /// The arguments of a call as the log keeps them, from what the model
    /// wrote.
    pub fn arguments(raw: &str, sensitive: bool) -> Value {
        if sensitive {
            return Self::redacted_arguments();
        }
        serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
    }

    /// `body` with the decided value withheld.
    pub fn body(&self, body: Value) -> Value {
        match self.decided {
            Some(value) => crate::suspend::withhold_secret(body, value),
            None => body,
        }
    }

    /// A model exchange as the log keeps it: in `request.messages`' assistant
    /// tool calls and in the answer's `tool_calls`, the arguments of every
    /// call to a tool `sensitive` names replaced by the marker the call's
    /// `tool_result` keeps; then the decided value withheld from both.
    pub fn exchange(
        &self,
        request: &mut Value,
        tool_calls: &mut [Value],
        sensitive: impl Fn(&str) -> bool,
    ) {
        let redact = |call: &mut Value| {
            let name = call["function"]["name"].as_str().unwrap_or_default();
            if sensitive(name) {
                call["function"]["arguments"] = Self::redacted_arguments();
            }
        };
        if let Some(messages) = request.get_mut("messages").and_then(Value::as_array_mut) {
            for message in messages {
                if let Some(calls) = message.get_mut("tool_calls").and_then(Value::as_array_mut) {
                    calls.iter_mut().for_each(redact);
                }
            }
        }
        tool_calls.iter_mut().for_each(redact);
        if self.decided.is_some() {
            *request = self.body(std::mem::take(request));
            for call in tool_calls {
                *call = self.body(std::mem::take(call));
            }
        }
    }
}

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

/// Anchor conversation `conversation_id`'s chain head in agent `agent_id`'s
/// own chain, bounded like any event. Called when a turn of the conversation
/// ends; a failure is logged, and the conversation's newest events stay
/// unanchored until the next turn's anchor.
pub async fn anchor(db: &Pool, agent_id: &str, conversation_id: &str) {
    let anchored = tokio::time::timeout(
        WRITE_BOUND,
        agent_audit::anchor_conversation(db, agent_id, conversation_id),
    )
    .await;
    let error = match anchored {
        Ok(Ok(_)) => return,
        Ok(Err(err)) => LogError::Db(err),
        Err(_) => LogError::TimedOut,
    };
    tracing::error!(
        error = %error,
        agent = agent_id,
        conversation = conversation_id,
        "a conversation's activity chain could not be anchored"
    );
}

/// A model call made on an agent's behalf outside the round loop — the
/// conversation's compaction summary, the evaluation's rubric judge, the
/// vision fallback that describes an image — recorded as an `llm_exchange`
/// of the run it belongs to, `purpose` saying which.
pub struct SideExchange {
    pub purpose: &'static str,
    pub model: Option<String>,
    pub backend: Option<String>,
    /// The body exactly as sent.
    pub request: Value,
    pub status: Option<u16>,
    /// The answer as it came back, parsed when it was JSON.
    pub response: Value,
    pub error: Option<String>,
    started: Instant,
}

impl SideExchange {
    pub fn new(purpose: &'static str) -> Self {
        Self {
            purpose,
            model: None,
            backend: None,
            request: Value::Null,
            status: None,
            response: Value::Null,
            error: None,
            started: Instant::now(),
        }
    }

    /// The answer's raw bytes, as JSON when they parse and as text otherwise.
    pub fn answered(&mut self, status: u16, body: &[u8]) {
        self.status = Some(status);
        self.response = serde_json::from_slice(body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(body).into_owned()));
    }
}

/// The run a [`SideExchange`] is recorded in: its principal, call chain and
/// where in it the call happened. Only an agent's run has one, so a
/// person's compaction or vision fallback records nothing.
#[derive(Clone)]
pub struct RunLog {
    principal_id: String,
    chain: Arc<RunChain>,
    at: Correlation,
}

impl RunLog {
    /// The run of the call `ctx` belongs to; `None` for a person's turn.
    pub fn of(ctx: &ToolContext) -> Option<Self> {
        let run = ctx.agent.as_deref()?;
        Some(Self {
            principal_id: run.system_principal().id.clone(),
            chain: run.chain().clone(),
            at: ctx.correlation(),
        })
    }

    /// A conversation of `principal` at `version` that no run is driving
    /// right now — an evaluation case's, once its script ran.
    pub fn conversation(principal: &SystemPrincipal, version: i64, conversation: &str) -> Self {
        Self {
            principal_id: principal.id.clone(),
            chain: Arc::new(RunChain::root(
                conversation,
                None,
                Frame::for_principal(principal, Some(version)),
            )),
            at: Correlation {
                session_id: Some(conversation.to_string()),
                ..Correlation::default()
            },
        }
    }

    pub async fn record(&self, db: &Pool, exchange: SideExchange) {
        let latency = u64::try_from(exchange.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut detail = json!({
            "purpose": exchange.purpose,
            "model": exchange.model,
            "backend": exchange.backend,
            "request": exchange.request,
            "response": { "status": exchange.status, "body": exchange.response },
            "latency_ms": latency,
        });
        if let Some(error) = exchange.error {
            detail["error"] = json!(error);
        }
        let mut event = NewEvent::new(AuditKind::LlmExchange, &self.principal_id, detail)
            .in_run(Some(&self.chain))
            .at(self.at.clone());
        event.duration_ms = Some(latency);
        let _ = record_event(db, event).await;
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
