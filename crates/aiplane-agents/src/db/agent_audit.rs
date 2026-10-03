// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The agent activity log (`docs/agents.md`, "What #111 built"): an
//! append-only, hash-chained record of everything done to or by a system
//! principal, in the `agent_audit` table.
//!
//! **Two kinds of writer, one insert.** A management change (a grant, a
//! share, a publish) is recorded with [`record`] on the caller's own
//! transaction, so the change can never exist without its event. A run event
//! (a model exchange, a tool call, a gate) is recorded with [`append_now`],
//! which takes the database's write lock for exactly one event. Both end in
//! [`append`], the only `INSERT` into the table; the runtime reaches it
//! through `aiplane_runtime::agents::audit`, which bounds the wait and fails
//! a run closed when an event cannot be written.
//!
//! **Chains.** Every event belongs to one chain: the conversation it happened
//! in (`conversation:<root session>`, sub-agent runs below it included), or,
//! outside any conversation, its agent (`agent:<principal>`). Within a chain
//! events are numbered from 1 (`seq`), and each stores the hash of the one
//! before it (`prev_hash`) and its own `hash`: HMAC-SHA256 over its canonical
//! JSON ([`StoredEvent::canonical`]), which covers every column, under a key
//! derived from the gateway's at-rest key ([`install_key_ring`]), so the
//! database alone cannot forge a consistent chain. Each turn's end anchors
//! its conversation's head in the agent's own chain ([`anchor_conversation`]),
//! so a cut tail or a deleted conversation chain shows too. [`verify`] walks
//! the chains and reports the first link that does not hold. Rows written
//! before #111 have no chain and are reported as unchained.
//!
//! **No foreign keys** (`migrations/0077_system_principals.sql`): the log
//! outlives the principal, the acting user and the conversation, until the
//! retention sweep removes whole conversation chains ([`sweep_conversation_chains`]).

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

use jiff::Timestamp;
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;

use super::{DbError, Pool, WriteTx};
use aiplane_core::server::crypto::{ActivityKey, sha256_hex};
use aiplane_core::server::run_chain::RunChain;

pub mod exchange;
pub use exchange::Reconstructor;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditKind {
    PrincipalCreated,
    PrincipalDisabled,
    GrantAdded,
    GrantRemoved,
    TokenIssued,
    TokenRevoked,
    /// A tool call inside an agent run, allowed or denied.
    ToolCall,
    InjectionDetected,
    /// `forward_request`: every route's gate, and the route picked if any.
    RouteDecision,
    /// A sub-agent run started from a route.
    SubAgentDispatched,
    /// A sub-agent run ended, with its outcome.
    SubAgentFinished,
    /// The output filter (#89) redacted or withheld a main agent's answer.
    OutputBlocked,
    /// A run paused for a decision (an approval, a secure input). Never the
    /// value a resume brings.
    RunSuspended,
    /// A paused run was resumed: who answered, and the decision's shape.
    RunResumed,
    AgentCreated,
    AgentDraftUpdated,
    AgentPublished,
    AgentLiveVersionSet,
    AgentShareSet,
    AgentShareRemoved,
    AgentDeleted,
    EmbedKeyCreated,
    EmbedKeyRevoked,
    /// A visitor request refused by a rate limit or the agent's budget.
    LimitRefused,
    /// The retention sweeper deleted conversations; counts only.
    ConversationsSwept,
    /// The main agent handed the conversation to a human (`request_human`
    /// or a `human` route), with the run chain. #100's analytics count it.
    HumanHandoff,
    /// A responder was added to or removed from an agent's inbox.
    ResponderAdded,
    ResponderRemoved,
    /// A Slack or Discord notification channel was added or removed. The
    /// URL is never in the detail.
    ChannelCreated,
    ChannelDeleted,
    /// A verifier step (#95): a code sent, a code checked, a lookup made, and
    /// the outcome. Never the code, the address or a looked-up value.
    VerifierOutcome,
    /// A host identity token was accepted or refused (#95). Never a claim value.
    HostIdentity,
    /// An A2A caller started, answered or cancelled a task (#102): which
    /// caller, token, context and task. Never the message text.
    A2aTask,
    /// One iteration of a `loop` route (#103): the worker's and the critic's
    /// child runs, and whether the critic accepted.
    LoopIteration,
    /// A `loop` route ended: how many iterations, and why it stopped.
    LoopFinished,
    /// One request to a model and its whole answer (#111): the body sent,
    /// the assembled text, reasoning, tool calls, finish reason, usage,
    /// latency, or the error.
    LlmExchange,
    /// One tool call's full arguments and full result, before the prompt's
    /// byte budget trims it (#111).
    ToolResult,
    /// A turn of an agent run began: the message it answers.
    TurnStarted,
    /// A turn of an agent run ended: its status, answer or error.
    TurnFinished,
    /// A slot of the conversation's state was written: old and new value,
    /// provenance, writer.
    StateWritten,
    /// The retention sweeper deleted a whole conversation chain of the log.
    ActivitySwept,
    /// A conversation chain's head (`seq`, `hash`) at the end of a turn,
    /// kept in the agent's own chain.
    ChainAnchored,
    /// The retention sweep cut the agent's own chain before this event: the
    /// last removed event's `seq` and `hash` (where [`verify`] starts), how
    /// many went and when they were written, and the anchors among them
    /// that still guard a conversation.
    ChainCheckpoint,
}

impl AuditKind {
    pub const ALL: &'static [AuditKind] = &[
        Self::PrincipalCreated,
        Self::PrincipalDisabled,
        Self::GrantAdded,
        Self::GrantRemoved,
        Self::TokenIssued,
        Self::TokenRevoked,
        Self::ToolCall,
        Self::InjectionDetected,
        Self::RouteDecision,
        Self::SubAgentDispatched,
        Self::SubAgentFinished,
        Self::OutputBlocked,
        Self::RunSuspended,
        Self::RunResumed,
        Self::AgentCreated,
        Self::AgentDraftUpdated,
        Self::AgentPublished,
        Self::AgentLiveVersionSet,
        Self::AgentShareSet,
        Self::AgentShareRemoved,
        Self::AgentDeleted,
        Self::EmbedKeyCreated,
        Self::EmbedKeyRevoked,
        Self::LimitRefused,
        Self::ConversationsSwept,
        Self::HumanHandoff,
        Self::ResponderAdded,
        Self::ResponderRemoved,
        Self::ChannelCreated,
        Self::ChannelDeleted,
        Self::VerifierOutcome,
        Self::HostIdentity,
        Self::A2aTask,
        Self::LoopIteration,
        Self::LoopFinished,
        Self::LlmExchange,
        Self::ToolResult,
        Self::TurnStarted,
        Self::TurnFinished,
        Self::StateWritten,
        Self::ActivitySwept,
        Self::ChainAnchored,
        Self::ChainCheckpoint,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::PrincipalCreated => "principal_created",
            Self::PrincipalDisabled => "principal_disabled",
            Self::GrantAdded => "grant_added",
            Self::GrantRemoved => "grant_removed",
            Self::TokenIssued => "token_issued",
            Self::TokenRevoked => "token_revoked",
            Self::ToolCall => "tool_call",
            Self::InjectionDetected => "injection_detected",
            Self::RouteDecision => "route_decision",
            Self::SubAgentDispatched => "sub_agent_dispatched",
            Self::SubAgentFinished => "sub_agent_finished",
            Self::OutputBlocked => "output_blocked",
            Self::RunSuspended => "run_suspended",
            Self::RunResumed => "run_resumed",
            Self::AgentCreated => "agent_created",
            Self::AgentDraftUpdated => "agent_draft_updated",
            Self::AgentPublished => "agent_published",
            Self::AgentLiveVersionSet => "agent_live_version_set",
            Self::AgentShareSet => "agent_share_set",
            Self::AgentShareRemoved => "agent_share_removed",
            Self::AgentDeleted => "agent_deleted",
            Self::EmbedKeyCreated => "embed_key_created",
            Self::EmbedKeyRevoked => "embed_key_revoked",
            Self::LimitRefused => "limit_refused",
            Self::ConversationsSwept => "conversations_swept",
            Self::HumanHandoff => "human_handoff",
            Self::ResponderAdded => "responder_added",
            Self::ResponderRemoved => "responder_removed",
            Self::ChannelCreated => "channel_created",
            Self::ChannelDeleted => "channel_deleted",
            Self::VerifierOutcome => "verifier_outcome",
            Self::HostIdentity => "host_identity",
            Self::A2aTask => "a2a_task",
            Self::LoopIteration => "loop_iteration",
            Self::LoopFinished => "loop_finished",
            Self::LlmExchange => "llm_exchange",
            Self::ToolResult => "tool_result",
            Self::TurnStarted => "turn_started",
            Self::TurnFinished => "turn_finished",
            Self::StateWritten => "state_written",
            Self::ActivitySwept => "activity_swept",
            Self::ChainAnchored => "chain_anchored",
            Self::ChainCheckpoint => "chain_checkpoint",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.as_str() == s)
    }

    /// The kinds that carry a run's content — prompts, answers, tool payloads
    /// — rather than a decision about it. They can be large, so the decision
    /// trail ([`for_principal`]) leaves them out; the activity API reads them.
    pub fn is_content(self) -> bool {
        matches!(
            self,
            Self::LlmExchange | Self::ToolResult | Self::TurnStarted | Self::TurnFinished
        )
    }
}

/// Where in a run an event happened, beyond what its [`RunChain`] says.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Correlation {
    /// The run's own conversation: the visitor's, or a sub-agent's child
    /// session.
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
    /// The model round within the turn, from 0.
    pub round: Option<u32>,
    pub call_id: Option<String>,
    /// The root conversation of an event that has no run chain but belongs
    /// to a conversation all the same (a host identity, an A2A task). With
    /// a chain, the chain's root wins.
    pub conversation_id: Option<String>,
}

/// One event to append.
#[derive(Debug, Clone)]
pub struct NewEvent<'a> {
    pub kind: AuditKind,
    /// Who it happened to or by: the running frame's principal in a run.
    pub principal_id: &'a str,
    /// The person who caused it, when one did.
    pub actor_id: Option<&'a str>,
    pub chain: Option<&'a RunChain>,
    pub at: Correlation,
    pub duration_ms: Option<u64>,
    pub detail: Value,
}

impl<'a> NewEvent<'a> {
    pub fn new(kind: AuditKind, principal_id: &'a str, detail: Value) -> Self {
        Self {
            kind,
            principal_id,
            actor_id: None,
            chain: None,
            at: Correlation::default(),
            duration_ms: None,
            detail,
        }
    }

    pub fn by(mut self, actor_id: Option<&'a str>) -> Self {
        self.actor_id = actor_id;
        self
    }

    pub fn in_run(mut self, chain: Option<&'a RunChain>) -> Self {
        self.chain = chain;
        self
    }

    pub fn at(mut self, at: Correlation) -> Self {
        self.at = at;
        self
    }

    pub fn took(mut self, duration_ms: u64) -> Self {
        self.duration_ms = Some(duration_ms);
        self
    }

    /// The conversation this event's chain is: the run's root, else the
    /// one the caller named.
    fn conversation_id(&self) -> Option<String> {
        self.chain
            .map(|c| c.root_session.clone())
            .or_else(|| self.at.conversation_id.clone())
    }
}

/// The `key_id` of an event written with no key ring installed — only by a
/// process that never built the gateway's state (a unit test, a CLI). Where
/// a ring is installed, [`verify`] refuses such an event.
pub const UNKEYED: &str = "unkeyed";

static KEY_RING: RwLock<Vec<ActivityKey>> = RwLock::new(Vec::new());

/// The keys the log signs with (the first) and verifies with (any, by
/// `key_id`). Installed by the gateway's state from its at-rest key
/// (`Crypto::activity_keys`): process-wide, because every writer — the
/// management changes in this crate included — writes into the same chains.
pub fn install_key_ring(keys: Vec<ActivityKey>) {
    *KEY_RING.write().unwrap_or_else(|p| p.into_inner()) = keys;
}

fn key_ring() -> Vec<ActivityKey> {
    KEY_RING.read().unwrap_or_else(|p| p.into_inner()).clone()
}

/// The chain key of a conversation.
pub fn conversation_chain(conversation_id: &str) -> String {
    format!("conversation:{conversation_id}")
}

/// The chain key of everything an agent does outside a conversation.
pub fn agent_chain(principal_id: &str) -> String {
    format!("agent:{principal_id}")
}

/// One stored event with every column. `rowid` orders the whole table in
/// insertion order; the activity API pages by it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StoredEvent {
    pub rowid: i64,
    pub id: String,
    pub kind: String,
    pub principal_id: String,
    pub actor_id: Option<String>,
    pub agent_id: Option<String>,
    pub version: Option<i64>,
    pub conversation_id: Option<String>,
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
    pub round: Option<i64>,
    pub call_id: Option<String>,
    pub visitor_id: Option<String>,
    pub caller_id: Option<String>,
    pub duration_ms: Option<i64>,
    /// The serialized [`RunChain`], as stored.
    pub chain: Option<String>,
    /// The detail JSON, as stored.
    pub detail: String,
    pub created_at: String,
    pub chain_key: Option<String>,
    pub seq: Option<i64>,
    pub prev_hash: Option<String>,
    pub hash: Option<String>,
    /// Which key of the ring signed `hash`, or [`UNKEYED`].
    pub key_id: Option<String>,
}

impl StoredEvent {
    /// The text the event's hash is taken over: every column but the hash
    /// itself and the rowid, as one JSON object with its keys sorted and no
    /// whitespace. `chain` and `detail` go in as the exact text stored, so
    /// re-reading them can never change the bytes.
    pub fn canonical(&self) -> String {
        let fields = json!({
            "actor_id": self.actor_id,
            "agent_id": self.agent_id,
            "call_id": self.call_id,
            "caller_id": self.caller_id,
            "chain": self.chain,
            "chain_key": self.chain_key,
            "conversation_id": self.conversation_id,
            "created_at": self.created_at,
            "detail": self.detail,
            "duration_ms": self.duration_ms,
            "id": self.id,
            "key_id": self.key_id,
            "kind": self.kind,
            "prev_hash": self.prev_hash,
            "principal_id": self.principal_id,
            "round": self.round,
            "seq": self.seq,
            "session_id": self.session_id,
            "turn_id": self.turn_id,
            "version": self.version,
            "visitor_id": self.visitor_id,
        });
        canonical_json(&fields)
    }

    /// The hash this event must carry: HMAC-SHA256 under the ring key its
    /// `key_id` names, or plain SHA-256 for an unkeyed one. `None` when the
    /// ring does not hold that key.
    pub fn expected_hash(&self, ring: &[ActivityKey]) -> Option<String> {
        let canonical = self.canonical();
        match self.key_id.as_deref() {
            None | Some(UNKEYED) => Some(sha256_hex(canonical.as_bytes())),
            Some(id) => ring
                .iter()
                .find(|k| k.id == id)
                .map(|k| k.sign(canonical.as_bytes())),
        }
    }

    /// The event as the API and the export show it: the stored JSON parsed.
    pub fn to_json(&self) -> Value {
        let parse = |text: &str| serde_json::from_str(text).unwrap_or(Value::String(text.into()));
        json!({
            "cursor": self.rowid,
            "id": self.id,
            "kind": self.kind,
            "ts": self.created_at,
            "principal_id": self.principal_id,
            "actor_id": self.actor_id,
            "agent_id": self.agent_id,
            "version": self.version,
            "conversation_id": self.conversation_id,
            "session_id": self.session_id,
            "turn_id": self.turn_id,
            "round": self.round,
            "call_id": self.call_id,
            "visitor_id": self.visitor_id,
            "caller_id": self.caller_id,
            "duration_ms": self.duration_ms,
            "run_chain": self.chain.as_deref().map(parse),
            "detail": parse(&self.detail),
            "chain_key": self.chain_key,
            "seq": self.seq,
            "prev_hash": self.prev_hash,
            "hash": self.hash,
            "key_id": self.key_id,
        })
    }
}

/// JSON with every object's keys sorted and no whitespace, whatever order
/// `serde_json`'s map keeps — the hash must not depend on a crate feature.
fn canonical_json(v: &Value) -> String {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let body: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", Value::String(k.clone()), canonical_json(&map[k])))
                .collect();
            format!("{{{}}}", body.join(","))
        }
        Value::Array(items) => {
            let body: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", body.join(","))
        }
        other => other.to_string(),
    }
}

/// Where an appended event landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Appended {
    pub id: String,
    pub chain_key: String,
    pub seq: i64,
    pub hash: String,
}

/// Record a management change on `conn` — the caller's transaction, so the
/// event commits or rolls back with the change it records.
pub async fn record(
    conn: &mut WriteTx,
    kind: AuditKind,
    principal_id: &str,
    actor_id: &str,
    detail: Value,
) -> Result<(), DbError> {
    append(
        conn,
        NewEvent::new(kind, principal_id, detail).by(Some(actor_id)),
    )
    .await
    .map(|_| ())
}

/// A chain's newest event. Answered from `idx_agent_audit_chain_head`
/// alone: the row's own `hash` sits behind `detail`, which may span
/// overflow pages.
const HEAD_SQL: &str =
    "SELECT seq, hash FROM agent_audit WHERE chain_key = ? ORDER BY seq DESC LIMIT 1";

/// Each conversation chain of an agent with its conversation and newest
/// event, from `idx_agent_audit_sweep` alone.
const SWEEP_SQL: &str = "SELECT chain_key, conversation_id, MAX(rtrim(created_at, 'Z'))
                           FROM agent_audit
                          WHERE agent_id = ? AND chain_key LIKE 'conversation:%'
                          GROUP BY chain_key";

/// Append one event on `conn`, which holds the database's write lock for the
/// rest of its transaction ([`WriteTx`]): the chain's head is read and
/// extended under it, so two writers cannot both take the same place. The
/// unique index on `(chain_key, seq)` refuses a fork should one try.
pub async fn append(
    conn: &mut WriteTx,
    mut event: NewEvent<'_>,
) -> Result<Appended, DbError> {
    let conversation_id = event.conversation_id();
    let chain_key = match &conversation_id {
        Some(c) => conversation_chain(c),
        None => agent_chain(event.principal_id),
    };
    if event.kind == AuditKind::LlmExchange {
        let created_at = Timestamp::now().to_string();
        for (hash, data) in exchange::take_blobs(&mut event.detail) {
            sqlx::query(
                "INSERT INTO activity_blobs (chain_key, hash, data, created_at)
                 VALUES (?, ?, ?, ?) ON CONFLICT DO NOTHING",
            )
            .bind(&chain_key)
            .bind(&hash)
            .bind(&data)
            .bind(&created_at)
            .execute(&mut **conn)
            .await?;
        }
    }
    let head: Option<(i64, Option<String>)> = sqlx::query_as(HEAD_SQL)
        .bind(&chain_key)
        .fetch_optional(&mut **conn)
        .await?;
    let (seq, prev_hash) = match head {
        Some((seq, hash)) => (seq + 1, hash),
        None => (1, None),
    };
    let mut row = StoredEvent {
        rowid: 0,
        id: Uuid::new_v4().to_string(),
        kind: event.kind.as_str().to_string(),
        principal_id: event.principal_id.to_string(),
        actor_id: event.actor_id.map(str::to_string),
        agent_id: Some(
            event
                .chain
                .map_or(event.principal_id, |c| c.agent().principal_id.as_str())
                .to_string(),
        ),
        version: event.chain.and_then(|c| c.current().version),
        conversation_id,
        session_id: event.at.session_id,
        turn_id: event.at.turn_id,
        round: event.at.round.map(i64::from),
        call_id: event.at.call_id,
        visitor_id: event.chain.and_then(|c| c.visitor_id.clone()),
        caller_id: event
            .chain
            .and_then(|c| c.caller.as_ref())
            .map(|c| c.principal_id.clone()),
        duration_ms: event
            .duration_ms
            .map(|d| i64::try_from(d).unwrap_or(i64::MAX)),
        chain: event.chain.map(|c| c.to_json().to_string()),
        detail: event.detail.to_string(),
        created_at: Timestamp::now().to_string(),
        chain_key: Some(chain_key.clone()),
        seq: Some(seq),
        prev_hash,
        hash: None,
        key_id: None,
    };
    let ring = key_ring();
    row.key_id = Some(ring.first().map_or(UNKEYED, |k| k.id.as_str()).to_string());
    let hash = row
        .expected_hash(&ring)
        .expect("the signing key is the ring's own");
    row.hash = Some(hash.clone());
    sqlx::query(
        "INSERT INTO agent_audit (id, kind, principal_id, actor_id, chain, detail, created_at,
                                  chain_key, seq, prev_hash, hash, agent_id, version,
                                  conversation_id, session_id, turn_id, round, call_id,
                                  visitor_id, caller_id, duration_ms, key_id)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&row.id)
    .bind(&row.kind)
    .bind(&row.principal_id)
    .bind(&row.actor_id)
    .bind(&row.chain)
    .bind(&row.detail)
    .bind(&row.created_at)
    .bind(&row.chain_key)
    .bind(row.seq)
    .bind(&row.prev_hash)
    .bind(&row.hash)
    .bind(&row.agent_id)
    .bind(row.version)
    .bind(&row.conversation_id)
    .bind(&row.session_id)
    .bind(&row.turn_id)
    .bind(row.round)
    .bind(&row.call_id)
    .bind(&row.visitor_id)
    .bind(&row.caller_id)
    .bind(row.duration_ms)
    .bind(&row.key_id)
    .execute(&mut **conn)
    .await?;
    Ok(Appended {
        id: row.id,
        chain_key,
        seq,
        hash,
    })
}

/// Append one event in a transaction of its own that takes the write lock
/// first (`BEGIN IMMEDIATE`), so concurrent writers to one chain queue up
/// instead of racing. Waits at most the pool's busy timeout for the lock;
/// the runtime bounds the whole call (`agents::audit`).
pub async fn append_now(pool: &Pool, event: NewEvent<'_>) -> Result<Appended, DbError> {
    let mut tx = WriteTx::begin(pool).await?;
    let appended = append(&mut tx, event).await?;
    tx.commit().await?;
    Ok(appended)
}

/// Anchor conversation `conversation_id`'s chain in agent `agent_id`'s own
/// chain: a `chain_anchored` event with the head's `seq` and `hash`, read
/// and written in one write transaction so the head cannot move in between.
/// `Ok(None)` when the conversation has no events.
pub async fn anchor_conversation(
    pool: &Pool,
    agent_id: &str,
    conversation_id: &str,
) -> Result<Option<Appended>, DbError> {
    let key = conversation_chain(conversation_id);
    let mut tx = WriteTx::begin(pool).await?;
    let head: Option<(i64, Option<String>)> = sqlx::query_as(HEAD_SQL)
        .bind(&key)
        .fetch_optional(&mut *tx)
        .await?;
    let Some((seq, hash)) = head else {
        return Ok(None);
    };
    let appended = append(
        &mut tx,
        NewEvent::new(
            AuditKind::ChainAnchored,
            agent_id,
            json!({ "chain_key": key, "seq": seq, "hash": hash }),
        ),
    )
    .await?;
    tx.commit().await?;
    Ok(Some(appended))
}

const COLUMNS: &str = "rowid, id, kind, principal_id, actor_id, agent_id, version, \
                       conversation_id, session_id, turn_id, round, call_id, visitor_id, \
                       caller_id, duration_ms, chain, detail, created_at, chain_key, seq, \
                       prev_hash, hash, key_id";

fn stored(row: &sqlx::sqlite::SqliteRow) -> Result<StoredEvent, DbError> {
    Ok(StoredEvent {
        rowid: row.try_get("rowid")?,
        id: row.try_get("id")?,
        kind: row.try_get("kind")?,
        principal_id: row.try_get("principal_id")?,
        actor_id: row.try_get("actor_id")?,
        agent_id: row.try_get("agent_id")?,
        version: row.try_get("version")?,
        conversation_id: row.try_get("conversation_id")?,
        session_id: row.try_get("session_id")?,
        turn_id: row.try_get("turn_id")?,
        round: row.try_get("round")?,
        call_id: row.try_get("call_id")?,
        visitor_id: row.try_get("visitor_id")?,
        caller_id: row.try_get("caller_id")?,
        duration_ms: row.try_get("duration_ms")?,
        chain: row.try_get("chain")?,
        detail: row.try_get("detail")?,
        created_at: row.try_get("created_at")?,
        chain_key: row.try_get("chain_key")?,
        seq: row.try_get("seq")?,
        prev_hash: row.try_get("prev_hash")?,
        hash: row.try_get("hash")?,
        key_id: row.try_get("key_id")?,
    })
}

/// A decision-trail event, parsed.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditEvent {
    pub id: String,
    pub kind: String,
    pub principal_id: String,
    pub actor_id: Option<String>,
    /// The run's call chain on a run event; `None` on a management event.
    pub chain: Option<Value>,
    pub detail: Value,
    pub created_at: Timestamp,
}

impl AuditEvent {
    fn of(e: StoredEvent) -> Result<Self, DbError> {
        Ok(Self {
            id: e.id,
            kind: e.kind,
            principal_id: e.principal_id,
            actor_id: e.actor_id,
            chain: e
                .chain
                .map(|c| serde_json::from_str(&c).unwrap_or(Value::String(c))),
            detail: serde_json::from_str(&e.detail).unwrap_or(Value::String(e.detail)),
            created_at: super::parse_ts(e.created_at, "created_at")?,
        })
    }
}

fn content_kinds() -> String {
    AuditKind::ALL
        .iter()
        .filter(|k| k.is_content())
        .map(|k| format!("'{}'", k.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `principal_id`'s decision trail, newest first: every event attributed to
/// it except the content kinds ([`AuditKind::is_content`]).
pub async fn for_principal(pool: &Pool, principal_id: &str) -> Result<Vec<AuditEvent>, DbError> {
    let sql = format!(
        "SELECT {COLUMNS} FROM agent_audit
          WHERE principal_id = ? AND kind NOT IN ({})
          ORDER BY rowid DESC",
        content_kinds()
    );
    let rows = sqlx::query(&sql).bind(principal_id).fetch_all(pool).await?;
    rows.iter().map(|r| AuditEvent::of(stored(r)?)).collect()
}

/// Which way a page runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Order {
    /// Oldest first: the timeline.
    Asc,
    /// Newest first.
    #[default]
    Desc,
}

/// A page of one agent's activity: its conversations (with every sub-agent
/// run below them) and its own chain.
#[derive(Debug, Clone, Default)]
pub struct ActivityQuery {
    pub agent_id: String,
    pub conversation_id: Option<String>,
    pub kinds: Vec<AuditKind>,
    pub from: Option<Timestamp>,
    pub to: Option<Timestamp>,
    /// The `rowid` the previous page ended at.
    pub cursor: Option<i64>,
    pub order: Order,
    pub limit: usize,
    /// Stop once this many bytes of `detail` are on the page (always at
    /// least one event), so a page of model exchanges stays bounded.
    pub max_bytes: usize,
}

#[derive(Debug, Clone, Default)]
pub struct ActivityPage {
    pub events: Vec<StoredEvent>,
    /// Pass back as `cursor` for the next page; `None` at the end.
    pub next_cursor: Option<i64>,
}

/// Rows fetched per query while a page fills, so a page of large events is
/// never read whole before the byte cap can stop it.
const PAGE_BATCH: usize = 16;

pub async fn page(pool: &Pool, q: &ActivityQuery) -> Result<ActivityPage, DbError> {
    let mut out = ActivityPage::default();
    let mut bytes = 0usize;
    let mut cursor = q.cursor;
    let limit = q.limit.max(1);
    loop {
        let want = PAGE_BATCH.min(limit - out.events.len());
        let batch = batch(pool, q, cursor, want).await?;
        let exhausted = batch.len() < want;
        for event in batch {
            cursor = Some(event.rowid);
            bytes += event.detail.len();
            out.events.push(event);
            if out.events.len() >= limit || bytes >= q.max_bytes {
                out.next_cursor = cursor;
                return Ok(out);
            }
        }
        if exhausted {
            return Ok(out);
        }
    }
}

async fn batch(
    pool: &Pool,
    q: &ActivityQuery,
    cursor: Option<i64>,
    limit: usize,
) -> Result<Vec<StoredEvent>, DbError> {
    let mut sql = format!("SELECT {COLUMNS} FROM agent_audit WHERE agent_id = ?");
    if q.conversation_id.is_some() {
        sql.push_str(" AND conversation_id = ?");
    }
    if !q.kinds.is_empty() {
        let list: Vec<String> = q
            .kinds
            .iter()
            .map(|k| format!("'{}'", k.as_str()))
            .collect();
        sql.push_str(&format!(" AND kind IN ({})", list.join(", ")));
    }
    if q.from.is_some() {
        sql.push_str(" AND rtrim(created_at, 'Z') >= ?");
    }
    if q.to.is_some() {
        sql.push_str(" AND rtrim(created_at, 'Z') < ?");
    }
    let (cmp, dir) = match q.order {
        Order::Asc => (">", "ASC"),
        Order::Desc => ("<", "DESC"),
    };
    if cursor.is_some() {
        sql.push_str(&format!(" AND rowid {cmp} ?"));
    }
    sql.push_str(&format!(" ORDER BY rowid {dir} LIMIT ?"));
    let mut query = sqlx::query(&sql).bind(&q.agent_id);
    if let Some(c) = &q.conversation_id {
        query = query.bind(c);
    }
    if let Some(from) = q.from {
        query = query.bind(super::window_key(from));
    }
    if let Some(to) = q.to {
        query = query.bind(super::window_key(to));
    }
    if let Some(c) = cursor {
        query = query.bind(c);
    }
    let rows = query
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(pool)
        .await?;
    rows.iter().map(stored).collect()
}

/// The first link of a chain that does not hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BrokenLink {
    pub chain_key: String,
    /// The `seq` the walk expected next.
    pub seq: i64,
    pub event_id: Option<String>,
    pub reason: String,
}

/// A chain's newest event, for an operator to keep outside the gateway.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChainHead {
    pub chain_key: String,
    pub seq: i64,
    pub hash: Option<String>,
}

/// What [`verify`] found for one agent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Verification {
    pub chains: u64,
    pub events: u64,
    /// Events recorded before #111, which belong to no chain.
    pub unchained: u64,
    /// Conversation events newer than their chain's latest anchor: written
    /// after the last turn ended (or by a turn whose anchor failed), so a
    /// removal of them would not show yet.
    pub unanchored: u64,
    /// The agent's own chain's newest event. Its `seq` and `hash` pin every
    /// anchor, so keeping them outside the gateway pins the whole log.
    pub head: Option<ChainHead>,
    pub broken: Option<BrokenLink>,
}

impl Verification {
    pub fn ok(&self) -> bool {
        self.broken.is_none()
    }
}

/// Rows read per query during [`verify`], so a long chain is walked in
/// bounded memory.
const VERIFY_BATCH: i64 = 256;

/// Where a walk of one chain ended.
struct Walked {
    last_seq: i64,
    last_hash: Option<String>,
}

/// Where a walk starts: after `seq`, whose hash is `hash` — `(0, None)` for a
/// chain that was never cut.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Base {
    seq: i64,
    hash: Option<String>,
}

/// Walk chain `key` link by link from `base`, handing every event to `on`.
/// `None` when a link does not hold; `out.broken` then says which.
async fn walk(
    pool: &Pool,
    key: &str,
    base: Base,
    ring: &[ActivityKey],
    out: &mut Verification,
    mut on: impl FnMut(&StoredEvent),
) -> Result<Option<Walked>, DbError> {
    out.chains += 1;
    let mut expected = base.seq + 1;
    let mut prev: Option<String> = base.hash;
    loop {
        let sql = format!(
            "SELECT {COLUMNS} FROM agent_audit WHERE chain_key = ? AND seq >= ?
              ORDER BY seq LIMIT ?"
        );
        let rows = sqlx::query(&sql)
            .bind(key)
            .bind(expected)
            .bind(VERIFY_BATCH)
            .fetch_all(pool)
            .await?;
        let done = (rows.len() as i64) < VERIFY_BATCH;
        for row in &rows {
            let event = stored(row)?;
            out.events += 1;
            let mut broken = |reason: String| {
                out.broken = Some(BrokenLink {
                    chain_key: key.to_string(),
                    seq: expected,
                    event_id: Some(event.id.clone()),
                    reason,
                });
            };
            if event.seq != Some(expected) {
                broken(format!(
                    "event {expected} of the chain is missing: the next one stored is {}",
                    event.seq.unwrap_or_default()
                ));
                return Ok(None);
            }
            if event.prev_hash != prev {
                broken("its prev_hash is not the hash of the event before it".into());
                return Ok(None);
            }
            let unkeyed = matches!(event.key_id.as_deref(), None | Some(UNKEYED));
            if unkeyed && !ring.is_empty() {
                broken(
                    "it is not signed with the gateway's log key: it was written or rewritten \
                     outside the gateway"
                        .into(),
                );
                return Ok(None);
            }
            match event.expected_hash(ring) {
                Some(hash) if event.hash.as_deref() == Some(hash.as_str()) => {}
                Some(_) => {
                    broken(
                        "its content does not match its hash: it was changed after it was \
                         written"
                            .into(),
                    );
                    return Ok(None);
                }
                None => {
                    broken(format!(
                        "it is signed with log key {}, which this gateway does not hold: the \
                         at-rest key was replaced without keeping the old one, or the event \
                         was forged",
                        event.key_id.as_deref().unwrap_or_default()
                    ));
                    return Ok(None);
                }
            }
            on(&event);
            prev = event.hash.clone();
            expected += 1;
        }
        if done {
            break;
        }
    }
    Ok(Some(Walked {
        last_seq: expected - 1,
        last_hash: prev,
    }))
}

/// Walk every chain of `agent_id` — its own first, then each conversation —
/// and check each link: `seq` counts up from 1 without a gap, `prev_hash` is
/// the hash of the event before, and `hash` is the HMAC of the event as
/// stored under the key it names. Then check each conversation chain
/// against its latest anchor in the agent's chain: it must still reach the
/// anchored `seq` with the anchored `hash`, and an anchored chain must exist
/// unless the retention sweep recorded removing it. Stops at the first
/// thing that does not hold.
///
/// What it cannot show: events removed from the tail of the agent's own
/// chain together with the conversation tails they anchored — compare
/// [`Verification::head`] with a copy kept elsewhere for that — nor
/// anything forged by someone holding the gateway's at-rest key.
pub async fn verify(pool: &Pool, agent_id: &str) -> Result<Verification, DbError> {
    let ring = key_ring();
    let mut out = Verification {
        unchained: sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM agent_audit WHERE agent_id = ? AND chain_key IS NULL",
        )
        .bind(agent_id)
        .fetch_one(pool)
        .await?
        .max(0) as u64,
        ..Verification::default()
    };
    let own = agent_chain(agent_id);
    let checkpoint: Option<String> = sqlx::query_scalar(LATEST_CHECKPOINT_SQL)
        .bind(&own)
        .fetch_optional(pool)
        .await?;
    let checkpoint: Value = checkpoint
        .and_then(|d| serde_json::from_str(&d).ok())
        .unwrap_or(Value::Null);
    let base = Base {
        seq: checkpoint["base_seq"].as_i64().unwrap_or(0),
        hash: checkpoint["base_hash"].as_str().map(str::to_string),
    };
    let base_seq = base.seq;
    let mut book = AnchorBook::default();
    book.carry(&checkpoint);
    let walked = walk(pool, &own, base, &ring, &mut out, |e| book.read(e)).await?;
    let AnchorBook { mut anchors, swept } = book;
    let Some(walked) = walked else {
        return Ok(out);
    };
    if walked.last_seq == base_seq {
        out.chains -= 1;
    } else {
        out.head = Some(ChainHead {
            chain_key: own.clone(),
            seq: walked.last_seq,
            hash: walked.last_hash,
        });
    }
    let mut keys: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT chain_key FROM agent_audit
          WHERE agent_id = ? AND chain_key IS NOT NULL AND chain_key != ?",
    )
    .bind(agent_id)
    .bind(&own)
    .fetch_all(pool)
    .await?;
    keys.sort();
    for key in &keys {
        let anchor = anchors.remove(key).filter(|_| !swept.contains(key));
        let mut at_anchor: Option<Option<String>> = None;
        let anchored_seq = anchor.as_ref().map_or(0, |(seq, _)| *seq);
        let Some(walked) = walk(pool, key, Base::default(), &ring, &mut out, |e| {
            if e.seq == Some(anchored_seq) {
                at_anchor = Some(e.hash.clone());
            }
        })
        .await?
        else {
            return Ok(out);
        };
        out.unanchored += (walked.last_seq - anchored_seq).max(0) as u64;
        if let Some((seq, hash)) = anchor {
            let problem = if walked.last_seq < seq {
                Some(format!(
                    "the chain ends at event {} but was anchored at event {seq}: its newest \
                     events were removed",
                    walked.last_seq
                ))
            } else if at_anchor != Some(hash) {
                Some(format!(
                    "event {seq} is not the event that was anchored: the chain was rewritten \
                     from there"
                ))
            } else {
                None
            };
            if let Some(reason) = problem {
                out.broken = Some(BrokenLink {
                    chain_key: key.clone(),
                    seq,
                    event_id: None,
                    reason,
                });
                return Ok(out);
            }
        }
    }
    let mut missing: Vec<(String, i64)> = anchors
        .into_iter()
        .filter(|(key, _)| !swept.contains(key))
        .map(|(key, (seq, _))| (key, seq))
        .collect();
    missing.sort();
    if let Some((key, seq)) = missing.into_iter().next() {
        out.broken = Some(BrokenLink {
            chain_key: key,
            seq,
            event_id: None,
            reason: format!(
                "the chain is gone, though it was anchored at event {seq} and no retention sweep \
                 removed it"
            ),
        });
    }
    Ok(out)
}

/// The newest checkpoint of a chain: where its stored part begins.
const LATEST_CHECKPOINT_SQL: &str = "SELECT detail FROM agent_audit
                                      WHERE chain_key = ? AND kind = 'chain_checkpoint'
                                      ORDER BY seq DESC LIMIT 1";

/// What an agent's own chain says about its conversation chains, read in
/// chain order: the latest anchor of each and which ones the sweep removed.
/// [`verify`] reads the whole stored chain into one; a cut reads the prefix
/// it removes, and its checkpoint carries the anchors that still matter.
#[derive(Default)]
struct AnchorBook {
    anchors: HashMap<String, (i64, Option<String>)>,
    swept: HashSet<String>,
}

impl AnchorBook {
    fn read(&mut self, e: &StoredEvent) {
        let detail: Value = serde_json::from_str(&e.detail).unwrap_or(Value::Null);
        match e.kind.as_str() {
            "chain_anchored" => {
                if let Some(chain) = detail["chain_key"].as_str() {
                    self.anchors.insert(
                        chain.to_string(),
                        (
                            detail["seq"].as_i64().unwrap_or(0),
                            detail["hash"].as_str().map(str::to_string),
                        ),
                    );
                }
            }
            "activity_swept" => {
                if let Some(chain) = detail["chain_key"].as_str() {
                    self.swept.insert(chain.to_string());
                }
            }
            "chain_checkpoint" => self.carry(&detail),
            _ => {}
        }
    }

    /// The anchors a checkpoint carried. They are older than any anchor the
    /// chain holds after the cut they were carried over, so they never
    /// replace one already read.
    fn carry(&mut self, checkpoint: &Value) {
        let Some(carried) = checkpoint["anchors"].as_object() else {
            return;
        };
        for (chain, anchor) in carried {
            self.anchors.entry(chain.clone()).or_insert((
                anchor["seq"].as_i64().unwrap_or(0),
                anchor["hash"].as_str().map(str::to_string),
            ));
        }
    }

    /// The anchors of chains not swept, as a checkpoint carries them.
    fn still_guarding(self) -> Value {
        let mut carried: Vec<_> = self
            .anchors
            .into_iter()
            .filter(|(chain, _)| !self.swept.contains(chain))
            .collect();
        carried.sort();
        Value::Object(
            carried
                .into_iter()
                .map(|(chain, (seq, hash))| (chain, json!({ "seq": seq, "hash": hash })))
                .collect(),
        )
    }
}

/// Cut the prefix of `agent_id`'s own chain written before `before`, so the
/// chain honours the same retention as its conversations: one transaction
/// appends a `chain_checkpoint` — the last removed event's `seq` and `hash`,
/// how many were removed, their `seq` range and when they were written, and
/// the anchors among them of conversations not swept — and deletes the
/// prefix. [`verify`] starts the chain at the newest checkpoint. Returns how
/// many events went.
async fn cut_agent_chain(pool: &Pool, agent_id: &str, before: Timestamp) -> Result<u64, DbError> {
    let own = agent_chain(agent_id);
    let cutoff = super::window_key(before);
    let mut tx = WriteTx::begin(pool).await?;
    let (first, head): (Option<i64>, Option<i64>) =
        sqlx::query_as("SELECT MIN(seq), MAX(seq) FROM agent_audit WHERE chain_key = ?")
            .bind(&own)
            .fetch_one(&mut *tx)
            .await?;
    let (Some(first), Some(head)) = (first, head) else {
        return Ok(0);
    };
    let kept: Option<i64> = sqlx::query_scalar(
        "SELECT MIN(seq) FROM agent_audit WHERE chain_key = ? AND rtrim(created_at, 'Z') >= ?",
    )
    .bind(&own)
    .bind(&cutoff)
    .fetch_one(&mut *tx)
    .await?;
    let last = kept.map_or(head, |k| k - 1);
    if last < first {
        return Ok(0);
    }
    let sql = format!(
        "SELECT {COLUMNS} FROM agent_audit
          WHERE chain_key = ? AND seq <= ?
            AND kind IN ('chain_anchored', 'activity_swept', 'chain_checkpoint')
          ORDER BY seq"
    );
    let mut book = AnchorBook::default();
    for row in sqlx::query(&sql)
        .bind(&own)
        .bind(last)
        .fetch_all(&mut *tx)
        .await?
    {
        book.read(&stored(&row)?);
    }
    let edge = |seq: i64| {
        sqlx::query_as::<_, (Option<String>, String)>(
            "SELECT hash, created_at FROM agent_audit WHERE chain_key = ? AND seq = ?",
        )
        .bind(own.clone())
        .bind(seq)
    };
    let (_, from) = edge(first).fetch_one(&mut *tx).await?;
    let (base_hash, to) = edge(last).fetch_one(&mut *tx).await?;
    let removed = u64::try_from(last - first + 1).unwrap_or(0);
    append(
        &mut tx,
        NewEvent::new(
            AuditKind::ChainCheckpoint,
            agent_id,
            json!({
                "base_seq": last,
                "base_hash": base_hash,
                "removed": removed,
                "seqs": [first, last],
                "from": from,
                "to": to,
                "before": before,
                "anchors": book.still_guarding(),
            }),
        ),
    )
    .await?;
    sqlx::query("DELETE FROM agent_audit WHERE chain_key = ? AND seq <= ?")
        .bind(&own)
        .bind(last)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(removed)
}

/// What one sweep of one agent's log removed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweptChains {
    pub chains: u64,
    pub events: u64,
    /// Events cut from the front of the agent's own chain.
    pub agent_events: u64,
}

/// Delete the conversation chains of `agent_id` whose newest event is older
/// than `before` and whose conversation no longer exists — whole chains
/// only, so no chain is ever left with a hole — and the agent's events from
/// before #111 (unchained) older than `before`. Each chain goes in its own
/// transaction, together with an `activity_swept` event in the agent's chain
/// that names it, so [`verify`] knows its anchors were let go on purpose.
/// First the agent's own chain is cut back to `before` behind a checkpoint
/// ([`cut_agent_chain`]), so the markers this sweep writes stay for a
/// retention period of their own.
pub async fn sweep_conversation_chains(
    pool: &Pool,
    agent_id: &str,
    before: Timestamp,
) -> Result<SweptChains, DbError> {
    let agent_events = cut_agent_chain(pool, agent_id, before).await?;
    let candidates: Vec<(String, Option<String>, String)> = sqlx::query_as(SWEEP_SQL)
        .bind(agent_id)
        .fetch_all(pool)
        .await?;
    let cutoff = super::window_key(before);
    let mut out = SweptChains {
        agent_events,
        ..SweptChains::default()
    };
    for (key, conversation, newest) in candidates {
        if newest >= cutoff {
            continue;
        }
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM chat_sessions WHERE id = ?)")
                .bind(&conversation)
                .fetch_one(pool)
                .await?;
        if exists {
            continue;
        }
        let mut tx = WriteTx::begin(pool).await?;
        let gone = sqlx::query("DELETE FROM agent_audit WHERE chain_key = ?")
            .bind(&key)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        sqlx::query("DELETE FROM activity_blobs WHERE chain_key = ?")
            .bind(&key)
            .execute(&mut *tx)
            .await?;
        append(
            &mut tx,
            NewEvent::new(
                AuditKind::ActivitySwept,
                agent_id,
                json!({ "chain_key": key, "events": gone, "before": before }),
            ),
        )
        .await?;
        tx.commit().await?;
        out.chains += 1;
        out.events += gone;
    }
    out.events += sqlx::query(
        "DELETE FROM agent_audit
          WHERE agent_id = ? AND chain_key IS NULL AND rtrim(created_at, 'Z') < ?",
    )
    .bind(agent_id)
    .bind(&cutoff)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(out)
}

#[cfg(test)]
mod tests;
