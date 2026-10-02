-- Serve agents over A2A (issue #102, docs/agents.md "What #102 built").
--
-- 1. A new grant kind, `a2a_caller`: the principal may call the agent whose
--    id is `ref` over A2A. `principal_grants` carries a CHECK on `kind`, and
--    SQLite cannot alter a CHECK in place, so the table is rebuilt. Nothing
--    references it, so the rebuild touches no other table.
-- 2. `a2a_contexts`: one row per A2A context, which is one principal-owned
--    agent conversation (`chat_sessions`), recording the caller that opened
--    it. Only that caller can read or continue it. The row goes with the
--    conversation (retention) and with the agent.

CREATE TABLE principal_grants_new (
    principal_id TEXT NOT NULL REFERENCES system_principals(id) ON DELETE CASCADE,
    kind         TEXT NOT NULL CHECK (kind IN ('tool', 'connector', 'skill', 'rag_collection', 'pool', 'a2a_caller')),
    ref          TEXT NOT NULL,
    granted_by   TEXT NOT NULL,
    granted_at   TEXT NOT NULL,
    PRIMARY KEY (principal_id, kind, ref)
) STRICT;

INSERT INTO principal_grants_new (principal_id, kind, ref, granted_by, granted_at)
    SELECT principal_id, kind, ref, granted_by, granted_at FROM principal_grants;

DROP TABLE principal_grants;

ALTER TABLE principal_grants_new RENAME TO principal_grants;

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
