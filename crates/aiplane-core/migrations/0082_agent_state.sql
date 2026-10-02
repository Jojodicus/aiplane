-- SPDX-License-Identifier: AGPL-3.0-only
-- Copyright (C) 2026 croit GmbH
--
-- Typed state of one agent conversation (issue #85, docs/agents.md §3 "State").
--
-- One row per slot that has been written. `value` is JSON that already passed
-- the slot's validator; `provenance` says who wrote it ('llm', 'verifier:<id>'
-- or 'host') and `set_at` when, because a gate may require both
-- (`{slot, provenance: "verifier:otp", max_age: 15m}`). A rewrite replaces the
-- row: a gate judges the current value and who vouched for it, never a history.
--
-- Rows are keyed by the chat session alone, not by who owns it, so this table
-- holds for user-owned and principal-owned sessions alike.

CREATE TABLE agent_state (
    session_id  TEXT NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
    slot        TEXT NOT NULL,
    value       TEXT NOT NULL,
    provenance  TEXT NOT NULL CHECK (provenance IN ('llm', 'host') OR provenance LIKE 'verifier:_%'),
    set_at      TEXT NOT NULL,
    PRIMARY KEY (session_id, slot)
) STRICT;
