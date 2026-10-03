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
//! before it (`prev_hash`) and its own `hash`: SHA-256 over its canonical
//! JSON ([`StoredEvent::canonical`]), which covers every column. [`verify`]
//! walks the chains and reports the first link that does not hold. Rows
//! written before #111 have no chain and are reported as unchained.
//!
//! **No foreign keys** (`migrations/0077_system_principals.sql`): the log
//! outlives the principal, the acting user and the conversation, until the
//! retention sweep removes whole conversation chains ([`sweep_conversation_chains`]).

use jiff::Timestamp;
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;

use super::{DbError, Pool};
use aiplane_core::server::crypto::sha256_hex;
use aiplane_core::server::run_chain::RunChain;

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
    /// The retention sweeper deleted whole conversation chains of the log.
    ActivitySwept,
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

    pub fn computed_hash(&self) -> String {
        sha256_hex(self.canonical().as_bytes())
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
    conn: &mut sqlx::SqliteConnection,
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

/// Append one event on `conn`, which must hold the database's write lock
/// for the rest of its transaction (a write already made in it, or
/// `BEGIN IMMEDIATE`): the chain's head is read and extended under it, so
/// two writers cannot both take the same place. The unique index on
/// `(chain_key, seq)` refuses a fork should one try.
pub async fn append(
    conn: &mut sqlx::SqliteConnection,
    event: NewEvent<'_>,
) -> Result<Appended, DbError> {
    let conversation_id = event.conversation_id();
    let chain_key = match &conversation_id {
        Some(c) => conversation_chain(c),
        None => agent_chain(event.principal_id),
    };
    let head: Option<(i64, Option<String>)> = sqlx::query_as(HEAD_SQL)
        .bind(&chain_key)
        .fetch_optional(&mut *conn)
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
    };
    let hash = row.computed_hash();
    row.hash = Some(hash.clone());
    sqlx::query(
        "INSERT INTO agent_audit (id, kind, principal_id, actor_id, chain, detail, created_at,
                                  chain_key, seq, prev_hash, hash, agent_id, version,
                                  conversation_id, session_id, turn_id, round, call_id,
                                  visitor_id, caller_id, duration_ms)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
    .execute(&mut *conn)
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
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let appended = append(&mut tx, event).await?;
    tx.commit().await?;
    Ok(appended)
}

const COLUMNS: &str = "rowid, id, kind, principal_id, actor_id, agent_id, version, \
                       conversation_id, session_id, turn_id, round, call_id, visitor_id, \
                       caller_id, duration_ms, chain, detail, created_at, chain_key, seq, \
                       prev_hash, hash";

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

/// What [`verify`] found for one agent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Verification {
    pub chains: u64,
    pub events: u64,
    /// Events recorded before #111, which belong to no chain.
    pub unchained: u64,
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

/// Walk every chain of `agent_id` — its conversations and its own — and
/// check each link: `seq` counts up from 1 without a gap, `prev_hash` is the
/// hash of the event before, and `hash` is the hash of the event as stored.
/// Stops at the first link that does not hold.
///
/// A chain detects an event changed, removed from its middle, or inserted
/// into it. It cannot tell that its newest events were removed, or that a
/// whole chain was; export the log and keep the newest hashes elsewhere for
/// that (`docs/agents.md`).
pub async fn verify(pool: &Pool, agent_id: &str) -> Result<Verification, DbError> {
    let mut out = Verification::default();
    let mut keys: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT chain_key FROM agent_audit
          WHERE agent_id = ? AND chain_key IS NOT NULL",
    )
    .bind(agent_id)
    .fetch_all(pool)
    .await?;
    keys.sort();
    out.unchained = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM agent_audit WHERE agent_id = ? AND chain_key IS NULL",
    )
    .bind(agent_id)
    .fetch_one(pool)
    .await?
    .max(0) as u64;
    for key in keys {
        out.chains += 1;
        let mut expected = 1i64;
        let mut prev: Option<String> = None;
        loop {
            let sql = format!(
                "SELECT {COLUMNS} FROM agent_audit WHERE chain_key = ? AND seq >= ?
                  ORDER BY seq LIMIT ?"
            );
            let rows = sqlx::query(&sql)
                .bind(&key)
                .bind(expected)
                .bind(VERIFY_BATCH)
                .fetch_all(pool)
                .await?;
            let done = (rows.len() as i64) < VERIFY_BATCH;
            for row in &rows {
                let event = stored(row)?;
                out.events += 1;
                let broken = |reason: String| BrokenLink {
                    chain_key: key.clone(),
                    seq: expected,
                    event_id: Some(event.id.clone()),
                    reason,
                };
                if event.seq != Some(expected) {
                    out.broken = Some(broken(format!(
                        "event {expected} of the chain is missing: the next one stored is {}",
                        event.seq.unwrap_or_default()
                    )));
                    return Ok(out);
                }
                if event.prev_hash != prev {
                    out.broken = Some(broken(
                        "its prev_hash is not the hash of the event before it".into(),
                    ));
                    return Ok(out);
                }
                if event.hash.as_deref() != Some(event.computed_hash().as_str()) {
                    out.broken = Some(broken(
                        "its content does not match its hash: it was changed after it was \
                         written"
                            .into(),
                    ));
                    return Ok(out);
                }
                prev = event.hash.clone();
                expected += 1;
            }
            if done {
                break;
            }
        }
    }
    Ok(out)
}

/// What one sweep of one agent's log removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweptChains {
    pub chains: u64,
    pub events: u64,
}

/// Delete the conversation chains of `agent_id` whose newest event is older
/// than `before` and whose conversation no longer exists — whole chains
/// only, so no chain is ever left with a hole — and the agent's events from
/// before #111 (unchained) older than `before`. The agent's own chain is
/// never swept. Each chain goes in its own transaction.
pub async fn sweep_conversation_chains(
    pool: &Pool,
    agent_id: &str,
    before: Timestamp,
) -> Result<SweptChains, DbError> {
    let candidates: Vec<(String, Option<String>, String)> = sqlx::query_as(SWEEP_SQL)
        .bind(agent_id)
        .fetch_all(pool)
        .await?;
    let cutoff = super::window_key(before);
    let mut out = SweptChains::default();
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
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
        let gone = sqlx::query("DELETE FROM agent_audit WHERE chain_key = ?")
            .bind(&key)
            .execute(&mut *tx)
            .await?
            .rows_affected();
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
