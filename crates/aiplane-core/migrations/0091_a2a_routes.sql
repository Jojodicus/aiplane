-- SPDX-License-Identifier: AGPL-3.0-only
-- Copyright (C) 2026 croit GmbH
--
-- A2A routes (issue #101, docs/agents.md "What #101 built"): a route may
-- dispatch to an external agent that speaks A2A.
--
-- `principal_grants` gains the kind `a2a_agent` next to 0090's `a2a_caller`;
-- its `ref` is the remote agent's card URL: a principal reaches an external
-- agent only when it is granted that card. SQLite cannot widen a CHECK in place, so the table is rebuilt
-- (migrations run with foreign keys off, see README.md).
--
-- `agent_a2a_tasks`: a remote task that stopped in `input-required` while the
-- route's call waits for the visitor's value. The resumed call reads the
-- remote task and context ids from here; nothing else needs them.

CREATE TABLE principal_grants_new (
    principal_id TEXT NOT NULL REFERENCES system_principals(id) ON DELETE CASCADE,
    kind         TEXT NOT NULL CHECK (kind IN ('tool', 'connector', 'skill', 'rag_collection', 'pool', 'a2a_caller', 'a2a_agent')),
    ref          TEXT NOT NULL,
    granted_by   TEXT NOT NULL,
    granted_at   TEXT NOT NULL,
    PRIMARY KEY (principal_id, kind, ref)
) STRICT;

INSERT INTO principal_grants_new (principal_id, kind, ref, granted_by, granted_at)
    SELECT principal_id, kind, ref, granted_by, granted_at FROM principal_grants;

DROP TABLE principal_grants;
ALTER TABLE principal_grants_new RENAME TO principal_grants;

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
