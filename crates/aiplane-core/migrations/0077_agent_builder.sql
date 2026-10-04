-- SPDX-License-Identifier: AGPL-3.0-only
-- Copyright (C) 2026 croit GmbH
--
-- The agent builder (docs/agents.md): system principals, agents, their runs on
-- the chat substrate, the public and A2A endpoints, human in the loop,
-- verifiers, evaluation and the signed activity log.
--
-- Four tables of the previous release change: `gateway_groups`,
-- `usage_events` and `mcp_tool_audit` gain columns in place, and
-- `chat_sessions` is rebuilt. Everything else is new.

-- ---------------------------------------------------------------------------
-- System principals
-- ---------------------------------------------------------------------------

-- A principal that is not a user (CI, an integration, every agent) lives in
-- its own table, so none of the person paths (default groups, empty
-- `allowed_groups` meaning "everyone", per-user MCP, memory, the OIDC upsert)
-- can ever see one by accident.
CREATE TABLE system_principals (
    id          TEXT PRIMARY KEY NOT NULL,
    name        TEXT NOT NULL UNIQUE,
    display     TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    created_by  TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    disabled_at TEXT
) STRICT;

-- Default deny: a principal holds exactly these rows. `granted_by` is audit
-- only, not a foreign key, so a manager leaving does not take the grant along.
-- `a2a_caller`: may call the agent `ref` over A2A. `a2a_agent`: may reach the
-- external agent whose card URL is `ref`. `pools`, for a `model` grant only:
-- the JSON array of pools it routes through (those the granting manager could
-- use for it); NULL routes through every pool serving it (an admin's grant).
CREATE TABLE principal_grants (
    principal_id TEXT NOT NULL REFERENCES system_principals(id) ON DELETE CASCADE,
    kind         TEXT NOT NULL CHECK (kind IN ('tool', 'connector', 'skill', 'rag_collection', 'model', 'a2a_caller', 'a2a_agent')),
    ref          TEXT NOT NULL,
    granted_by   TEXT NOT NULL,
    granted_at   TEXT NOT NULL,
    pools        TEXT,
    PRIMARY KEY (principal_id, kind, ref)
) STRICT;

-- Separate from `tokens` with its own `gws_` prefix: `require_bearer` routes
-- by prefix, so a user-token lookup can never return a principal and the
-- other way round.
CREATE TABLE system_tokens (
    id           TEXT PRIMARY KEY NOT NULL,
    principal_id TEXT NOT NULL REFERENCES system_principals(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    hash         TEXT NOT NULL UNIQUE,
    created_by   TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    last_used_at TEXT,
    expires_at   TEXT NOT NULL,
    revoked_at   TEXT
) STRICT;

CREATE INDEX idx_system_tokens_principal ON system_tokens (principal_id);

-- Like `is_admin`: a capability on a gateway group. `is_admin` implies it.
ALTER TABLE gateway_groups ADD COLUMN can_manage_agents INTEGER NOT NULL DEFAULT 0;

-- Who is billed: `user_id` keeps holding the subject (users.id or
-- system_principals.id) and `principal_kind` says which, so every existing
-- per-user query stays correct. `agent_id` is the main agent at the root of
-- the run's call chain, so one read sums what a visitor conversation cost its
-- owner; `chain` is that call chain, serialized. Both NULL outside a run.
ALTER TABLE usage_events ADD COLUMN principal_kind TEXT NOT NULL DEFAULT 'user';
ALTER TABLE mcp_tool_audit ADD COLUMN principal_kind TEXT NOT NULL DEFAULT 'user';
ALTER TABLE mcp_tool_audit ADD COLUMN chain TEXT;
ALTER TABLE usage_events ADD COLUMN agent_id TEXT;
ALTER TABLE usage_events ADD COLUMN chain TEXT;

-- The owner's monthly budget, read on every visitor message.
CREATE INDEX usage_events_agent ON usage_events(agent_id, created_at)
    WHERE agent_id IS NOT NULL;

-- ---------------------------------------------------------------------------
-- Agents
-- ---------------------------------------------------------------------------

-- An agent is a principal plus a spec. Publishing snapshots `draft_spec` as an
-- immutable `agent_versions` row, so editing a draft never changes what
-- visitors and parent agents run.
CREATE TABLE agents (
    principal_id  TEXT PRIMARY KEY NOT NULL REFERENCES system_principals(id) ON DELETE CASCADE,
    draft_spec    TEXT NOT NULL,
    live_version  INTEGER,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
) STRICT;

CREATE TABLE agent_versions (
    principal_id  TEXT NOT NULL REFERENCES agents(principal_id) ON DELETE CASCADE,
    version       INTEGER NOT NULL,
    spec          TEXT NOT NULL,
    published_by  TEXT NOT NULL,
    published_at  TEXT NOT NULL,
    PRIMARY KEY (principal_id, version)
) STRICT;

-- A `read` or `write` share only takes effect for a holder of
-- `can_manage_agents`; that is checked when the share is written and again on
-- every request, not here. A `respond` share answers the agent's inbox items
-- and needs no such permission.
CREATE TABLE agent_shares (
    principal_id  TEXT NOT NULL REFERENCES agents(principal_id) ON DELETE CASCADE,
    subject_kind  TEXT NOT NULL CHECK (subject_kind IN ('user', 'group')),
    subject_id    TEXT NOT NULL,
    access        TEXT NOT NULL CHECK (access IN ('respond', 'read', 'write')),
    PRIMARY KEY (principal_id, subject_kind, subject_id)
) STRICT;

CREATE INDEX idx_agent_shares_subject ON agent_shares (subject_kind, subject_id);

-- ---------------------------------------------------------------------------
-- Agent runs on the chat substrate
-- ---------------------------------------------------------------------------

-- A conversation is owned by a person OR a system principal. Every query that
-- lists or opens conversations filters on `user_id`, so a principal-owned row
-- is invisible to every person by construction. An agent conversation is
-- never shared (`shared = 1` makes it readable by any signed-in person).
-- `parent_turn_id` has no foreign key: a sub-agent run stays an auditable
-- record when its parent is compacted or deleted. `lang` is the visitor's
-- language, so a resumed run answers in it whoever answered the pause.
--
-- SQLite cannot relax NOT NULL or add a CHECK in place, so the table is
-- rebuilt. Its children keep naming `chat_sessions` and cascade from the new
-- table after the rename; this relies on the runner's foreign-keys-off mode
-- (see README.md), without which the DROP would cascade into all of them.
CREATE TABLE chat_sessions_new (
    id             TEXT PRIMARY KEY NOT NULL,
    user_id        TEXT REFERENCES users(id) ON DELETE CASCADE,
    principal_id   TEXT REFERENCES system_principals(id) ON DELETE CASCADE,
    parent_turn_id TEXT,
    agent_version  INTEGER,
    title          TEXT,
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL,
    shared         INTEGER NOT NULL DEFAULT 0,
    pinned         INTEGER NOT NULL DEFAULT 0,
    visitor_id     TEXT REFERENCES visitor_sessions(id) ON DELETE SET NULL,
    lang           TEXT,
    CHECK ((user_id IS NULL) != (principal_id IS NULL)),
    CHECK (principal_id IS NULL OR shared = 0)
);

INSERT INTO chat_sessions_new (id, user_id, title, created_at, updated_at, shared, pinned)
SELECT id, user_id, title, created_at, updated_at, shared, pinned
FROM chat_sessions;

DROP TABLE chat_sessions;
ALTER TABLE chat_sessions_new RENAME TO chat_sessions;

CREATE INDEX chat_sessions_user_updated ON chat_sessions(user_id, updated_at DESC);
CREATE INDEX chat_sessions_principal_updated ON chat_sessions(principal_id, updated_at DESC);
CREATE INDEX chat_sessions_parent_turn ON chat_sessions(parent_turn_id)
    WHERE parent_turn_id IS NOT NULL;

-- A turn paused at a tool call that waits for a decision from outside the
-- model (an approval, a secret, a human's answer), written down so the pause
-- survives a restart and frees the worker. While a row exists the turn is
-- 'suspended'; the row is deleted in the same transaction that resumes it, so
-- a decision is claimed exactly once.
--
--   tool_call    JSON {id, name, arguments}: the call that waits
--   tail         JSON array: this turn's round messages, without that result
--   budget_used  JSON {rounds, seconds, tokens} spent before the pause
--   child_turn   the sub-agent turn that actually waits, resumed first
--   expires_at   RFC 3339; compared after parsing, never as a string
--   run_context  JSON {route, route_binds} of a sub-agent run: the values its
--                route bound at dispatch exist nowhere else
--   notified_at  set once, `WHERE notified_at IS NULL`, so a pause notifies
--                exactly once
CREATE TABLE chat_turn_suspensions (
    turn_id      TEXT PRIMARY KEY NOT NULL REFERENCES chat_turns(id) ON DELETE CASCADE,
    request_id   TEXT NOT NULL UNIQUE,
    kind         TEXT NOT NULL,
    message      TEXT,
    tool_call    TEXT NOT NULL,
    tail         TEXT NOT NULL,
    budget_used  TEXT NOT NULL,
    child_turn   TEXT,
    on_timeout   TEXT NOT NULL,
    expires_at   TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    run_context  TEXT,
    notified_at  TEXT
) STRICT;

-- The 30-second expiry sweep reads only the expired rows.
CREATE INDEX chat_turn_suspensions_expires_at ON chat_turn_suspensions(expires_at);

-- Typed state of one conversation, one row per written slot. A rewrite
-- replaces the row: a gate judges the current value and who vouched for it
-- (`provenance`, `set_at`), never a history.
CREATE TABLE agent_state (
    session_id  TEXT NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
    slot        TEXT NOT NULL,
    value       TEXT NOT NULL,
    provenance  TEXT NOT NULL CHECK (provenance IN ('llm', 'host') OR provenance LIKE 'verifier:_%'),
    set_at      TEXT NOT NULL,
    PRIMARY KEY (session_id, slot)
) STRICT;

-- ---------------------------------------------------------------------------
-- Public endpoint: embed keys and visitor sessions
-- ---------------------------------------------------------------------------

-- The embed key (`gwe_…`) ships in the host site's page source, so it is not
-- a secret, but it is stored hashed like every credential. `origins` is a
-- JSON array of exact `scheme://host[:port]`.
CREATE TABLE agent_embed_keys (
    id           TEXT PRIMARY KEY NOT NULL,
    principal_id TEXT NOT NULL REFERENCES agents(principal_id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    key_hash     TEXT NOT NULL UNIQUE,
    origins      TEXT NOT NULL,
    created_by   TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    revoked_at   TEXT
) STRICT;

CREATE INDEX agent_embed_keys_principal ON agent_embed_keys(principal_id);

-- `expires_at` slides by `idle_ttl_secs` on every request and never passes
-- `max_expires_at`, the absolute cap.
CREATE TABLE visitor_sessions (
    id             TEXT PRIMARY KEY NOT NULL,
    principal_id   TEXT NOT NULL REFERENCES agents(principal_id) ON DELETE CASCADE,
    embed_key_id   TEXT NOT NULL REFERENCES agent_embed_keys(id) ON DELETE CASCADE,
    token_hash     TEXT NOT NULL UNIQUE,
    session_id     TEXT NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
    client_ip      TEXT,
    idle_ttl_secs  INTEGER NOT NULL CHECK (idle_ttl_secs > 0),
    created_at     TEXT NOT NULL,
    last_seen_at   TEXT NOT NULL,
    expires_at     TEXT NOT NULL,
    max_expires_at TEXT NOT NULL
) STRICT;

CREATE INDEX visitor_sessions_principal ON visitor_sessions(principal_id);
-- The per-IP rate window, read on every visitor message.
CREATE INDEX visitor_sessions_ip ON visitor_sessions(principal_id, client_ip);
-- The rate windows join on it, and deleting a chat session cascades by it.
CREATE INDEX visitor_sessions_session ON visitor_sessions(session_id);

-- ---------------------------------------------------------------------------
-- Human in the loop
-- ---------------------------------------------------------------------------

-- A Slack or Discord incoming webhook, sealed at rest: the URL is the
-- credential.
CREATE TABLE agent_notify_channels (
    id            TEXT PRIMARY KEY NOT NULL,
    principal_id  TEXT NOT NULL REFERENCES agents(principal_id) ON DELETE CASCADE,
    kind          TEXT NOT NULL CHECK (kind IN ('slack', 'discord')),
    name          TEXT NOT NULL,
    url_nonce     BLOB NOT NULL,
    url_ct        BLOB NOT NULL,
    url_host      TEXT NOT NULL,
    details       INTEGER NOT NULL DEFAULT 0 CHECK (details IN (0, 1)),
    lang          TEXT NOT NULL DEFAULT 'en',
    created_by    TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    UNIQUE (principal_id, name)
) STRICT;

CREATE INDEX agent_notify_channels_principal ON agent_notify_channels(principal_id);

-- ---------------------------------------------------------------------------
-- Verifiers and rate windows
-- ---------------------------------------------------------------------------

-- The code itself is stored nowhere: the customer's connector generates and
-- checks it. `email_hash` ties a check to the address the code went to, so
-- changing the email slot in between cannot redirect a code.
CREATE TABLE agent_verifier_codes (
    session_id  TEXT NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
    verifier    TEXT NOT NULL,
    email_hash  TEXT NOT NULL,
    sent_at     TEXT NOT NULL,
    expires_at  TEXT NOT NULL,
    attempts    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (session_id, verifier)
) STRICT;

-- Host identity tokens already accepted, so one cannot be presented twice.
CREATE TABLE agent_identity_jtis (
    principal_id TEXT NOT NULL REFERENCES system_principals(id) ON DELETE CASCADE,
    jti_hash     TEXT NOT NULL,
    expires_at   TEXT NOT NULL,
    PRIMARY KEY (principal_id, jti_hash)
) STRICT;

-- One row per sliding window an event counts in (`counter` names the
-- window's subject). Checking and recording happen in one write transaction,
-- so parallel requests cannot all pass before any is counted. The times are
-- `db::window_key` text, which orders as a string.
CREATE TABLE rate_events (
    principal_id TEXT NOT NULL REFERENCES system_principals(id) ON DELETE CASCADE,
    counter      TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    expires_at   TEXT NOT NULL
) STRICT;

CREATE INDEX rate_events_by_counter ON rate_events (principal_id, counter, created_at);
CREATE INDEX rate_events_by_expiry ON rate_events (principal_id, expires_at);

-- ---------------------------------------------------------------------------
-- Evaluation
-- ---------------------------------------------------------------------------

CREATE TABLE agent_test_cases (
    id           TEXT PRIMARY KEY NOT NULL,
    principal_id TEXT NOT NULL REFERENCES agents(principal_id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    script       TEXT NOT NULL,
    expect       TEXT NOT NULL,
    rubric       TEXT,
    created_by   TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    UNIQUE (principal_id, name)
) STRICT;

-- `spec_hash` and `suite_hash` pin what a run tested: publishing accepts only
-- a green run of the draft and the suite as they are now.
CREATE TABLE agent_test_runs (
    id           TEXT PRIMARY KEY NOT NULL,
    principal_id TEXT NOT NULL REFERENCES agents(principal_id) ON DELETE CASCADE,
    source       TEXT NOT NULL,
    version      INTEGER,
    spec_hash    TEXT NOT NULL,
    suite_hash   TEXT NOT NULL,
    started_by   TEXT NOT NULL,
    started_at   TEXT NOT NULL,
    finished_at  TEXT NOT NULL
) STRICT;

CREATE INDEX idx_agent_test_runs_principal ON agent_test_runs (principal_id, started_at);

-- `session_id` has no foreign key: retention sweeps the test conversation,
-- and the stored report outlives it.
CREATE TABLE agent_test_results (
    run_id     TEXT NOT NULL REFERENCES agent_test_runs(id) ON DELETE CASCADE,
    case_id    TEXT NOT NULL,
    case_name  TEXT NOT NULL,
    position   INTEGER NOT NULL,
    passed     INTEGER NOT NULL CHECK (passed IN (0, 1)),
    report     TEXT NOT NULL,
    session_id TEXT,
    PRIMARY KEY (run_id, case_id)
) STRICT;

-- ---------------------------------------------------------------------------
-- A2A
-- ---------------------------------------------------------------------------

-- One A2A context is one agent conversation; only the caller that opened it
-- can read or continue it.
CREATE TABLE a2a_contexts (
    session_id  TEXT PRIMARY KEY NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
    agent_id    TEXT NOT NULL REFERENCES agents(principal_id) ON DELETE CASCADE,
    caller_id   TEXT NOT NULL REFERENCES system_principals(id) ON DELETE CASCADE,
    caller_name TEXT NOT NULL,
    token_id    TEXT NOT NULL,
    client_ip   TEXT,
    created_at  TEXT NOT NULL
) STRICT;

CREATE INDEX idx_a2a_contexts_agent_ip ON a2a_contexts (agent_id, client_ip);

-- A remote task stopped in `input-required` while the route's call waits for
-- the visitor; the resumed call continues it from here.
CREATE TABLE agent_a2a_tasks (
    turn_id      TEXT NOT NULL REFERENCES chat_turns(id) ON DELETE CASCADE,
    tool_call_id TEXT NOT NULL,
    route        TEXT NOT NULL,
    card_url     TEXT NOT NULL,
    task_id      TEXT NOT NULL,
    context_id   TEXT,
    created_at   TEXT NOT NULL,
    PRIMARY KEY (turn_id, tool_call_id)
) STRICT;

-- ---------------------------------------------------------------------------
-- Activity log
-- ---------------------------------------------------------------------------

-- Append-only trail of everything done to or by a principal. No foreign keys:
-- it must outlive the principal and the acting user and must not be erasable
-- by a cascade. Every event sits in a hash chain, one per conversation
-- (`conversation:<root session id>`) and one per agent for the rest
-- (`agent:<principal id>`); `hash` is HMAC-SHA256 over the event's canonical
-- JSON, `prev_hash` included, under the log key `key_id` names.
CREATE TABLE agent_audit (
    id              TEXT PRIMARY KEY NOT NULL,
    kind            TEXT NOT NULL,
    principal_id    TEXT NOT NULL,
    actor_id        TEXT,
    chain           TEXT,
    detail          TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    chain_key       TEXT,
    seq             INTEGER,
    prev_hash       TEXT,
    hash            TEXT,
    agent_id        TEXT,
    version         INTEGER,
    conversation_id TEXT,
    session_id      TEXT,
    turn_id         TEXT,
    round           INTEGER,
    call_id         TEXT,
    visitor_id      TEXT,
    caller_id       TEXT,
    duration_ms     INTEGER,
    key_id          TEXT
) STRICT;

-- `detail` spills onto overflow pages for a model exchange, so every query
-- that does not need it is answered from an index.
CREATE INDEX idx_agent_audit_principal ON agent_audit (principal_id, created_at DESC);
CREATE INDEX idx_agent_audit_principal_kind
    ON agent_audit (principal_id, kind, created_at);
-- The chain's order, and the guard against two writers forking it.
CREATE UNIQUE INDEX idx_agent_audit_chain ON agent_audit (chain_key, seq);
-- The chain's head for every append, without reading the row.
CREATE INDEX idx_agent_audit_chain_head ON agent_audit (chain_key, seq, hash);
-- Paging one agent's or one conversation's events by rowid.
CREATE INDEX idx_agent_audit_agent ON agent_audit (agent_id);
CREATE INDEX idx_agent_audit_conversation ON agent_audit (conversation_id);
-- The retention sweep: each conversation chain of an agent and its newest
-- event.
CREATE INDEX idx_agent_audit_sweep
    ON agent_audit (agent_id, chain_key, conversation_id, created_at);

-- Large binary parts of model exchanges, stored once per chain by content
-- hash; the hash is inside the event's signed hash, and a blob that no longer
-- matches it is not served.
CREATE TABLE activity_blobs (
    chain_key  TEXT NOT NULL,
    hash       TEXT NOT NULL,
    data       TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (chain_key, hash)
) STRICT, WITHOUT ROWID;

-- How far each chain was last found sound, so a check resumes there. `mac`
-- signs chain_key, seq, hash and book under the log key, so the database
-- alone cannot move a watermark forward over a change.
CREATE TABLE activity_verified (
    chain_key   TEXT PRIMARY KEY NOT NULL,
    seq         INTEGER NOT NULL,
    hash        TEXT,
    book        TEXT,
    key_id      TEXT NOT NULL,
    mac         TEXT NOT NULL,
    verified_at TEXT NOT NULL
) STRICT;

-- ---------------------------------------------------------------------------
-- The agent architect (#118)
-- ---------------------------------------------------------------------------

-- A person's chat that runs as the architect: its system prompt and its tools
-- instead of the person's chat tools. `agent_id` is the agent it plans, NULL
-- until it creates one.
CREATE TABLE agent_architect_sessions (
    session_id  TEXT PRIMARY KEY NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
    user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    agent_id    TEXT REFERENCES agents(principal_id) ON DELETE SET NULL,
    created_at  TEXT NOT NULL
) STRICT;

CREATE INDEX agent_architect_sessions_by_user
    ON agent_architect_sessions (user_id, agent_id, created_at);

-- The draft as it was before each save, so a change can be undone. Only the
-- newest few per agent are kept. `granted` lists the grants the save's change
-- made (`[{kind, ref}]`), which undoing it revokes unless something still
-- uses them.
CREATE TABLE agent_draft_revisions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    principal_id TEXT NOT NULL REFERENCES agents(principal_id) ON DELETE CASCADE,
    spec        TEXT NOT NULL,
    granted     TEXT NOT NULL DEFAULT '[]',
    saved_by    TEXT NOT NULL,
    saved_at    TEXT NOT NULL
) STRICT;

CREATE INDEX agent_draft_revisions_by_agent ON agent_draft_revisions (principal_id, id);
