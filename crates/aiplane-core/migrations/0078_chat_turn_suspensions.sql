-- SPDX-License-Identifier: AGPL-3.0-only
-- Copyright (C) 2026 croit GmbH
--
-- A turn paused at a tool call that is waiting for a decision from outside
-- the model: an approval, a secret the user types, an answer from a human.
--
-- `FeedbackHub` already parks a call while the turn lives in memory, which is
-- enough for `ask_user` and browser control. An approval can take hours, and
-- the process may restart in between, so the paused run is written down here
-- and the worker is freed. Resuming re-enters the same turn through the same
-- driver: the replayed history is rebuilt as for any turn, `tail` is appended
-- after it, and the decision becomes the pending call's tool result.
--
-- While a row exists, `chat_turns.status` is 'suspended'. The row is deleted
-- in the same transaction that flips the turn back to 'in_progress', so a
-- decision can be claimed exactly once, by one resume.
--
--   request_id   what the client answers; unique so a stale answer to an
--                earlier suspension of the same turn is refused
--   kind         approval | secure_input | human_answer
--   message      what the tool wants shown next to the decision, if anything
--   tool_call    JSON {id, name, arguments}: the call that is waiting
--   tail         JSON array: this turn's round messages so far, without the
--                waiting call's result
--   budget_used  JSON {rounds, seconds, tokens} spent before the pause
--   child_turn   set when the pause is inside a sub-agent run: the child turn
--                that actually waits, resumed first
--   on_timeout   deny | allow_once: what an expired suspension resolves to
--   expires_at   RFC 3339; compared after parsing, never as a string

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
    created_at   TEXT NOT NULL
) STRICT;
