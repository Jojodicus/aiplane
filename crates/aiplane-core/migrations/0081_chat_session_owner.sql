-- SPDX-License-Identifier: AGPL-3.0-only
-- Copyright (C) 2026 croit GmbH
--
-- A conversation is owned by a person OR a system principal (issue #83,
-- docs/agents.md §3 "Where runs live").
--
-- Agent runs reuse the chat substrate, but they are not chats of the agent's
-- owner. Every query that lists, searches or opens conversations filters on
-- `user_id`, so a row with `user_id IS NULL` is invisible to every person
-- without any extra code: separation by construction, not by a guard.
--
--   principal_id    the system principal that owns the run
--   parent_turn_id  on a sub-agent run: the main agent's turn that called it.
--                   No foreign key: the parent may be compacted or deleted and
--                   the child run is still an auditable record of its own.
--   agent_version   the published agent version the run executed, when the
--                   principal is an agent
--
-- A visitor session column (`visitor_id`) is left for #91, which creates the
-- `visitor_sessions` table it references.
--
-- The CHECKs: exactly one owner, and an agent conversation is never shared
-- (`shared = 1` makes a conversation readable by any signed-in person).
--
-- SQLite cannot relax NOT NULL or add a CHECK in place, so the table is
-- rebuilt. Seven tables reference `chat_sessions(id) ON DELETE CASCADE`, and
-- with foreign keys on, `DROP TABLE` deletes every row first and cascades.
-- `db::open` therefore runs migrations with foreign keys off and checks them
-- afterwards (see migrations/README.md). The child tables keep naming
-- `chat_sessions`, which after the rename is this table again.

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

-- The run's call chain (agent → sub-agent → tool), serialized, on every MCP
-- tool call made inside an agent run. NULL outside one.
ALTER TABLE mcp_tool_audit ADD COLUMN chain TEXT;
