-- SPDX-License-Identifier: AGPL-3.0-only
-- Copyright (C) 2026 croit GmbH
--
-- The events every sliding-window rate of an agent counts, in one table
-- (docs/agents.md, "What #92 built" and "What #95 built"): a visitor or A2A
-- request admitted, a verifier's code sent, a verifier's lookup made.
--
-- One row per window an event counts in: `counter` names the window's
-- subject (`conversation:visitor:<id>`, `ip:<address>`,
-- `verifier:<id>:send:email:<hash>`, ...). An event is checked against its
-- windows and recorded in one write transaction, so parallel requests
-- cannot all pass the check before any of them is counted.
--
-- `created_at` and `expires_at` hold `db::window_key` text (RFC 3339 without
-- the `Z`), which orders as a string. A row expires one window after it was
-- written; recording an event drops the agent's expired rows.
--
-- Replaces `agent_verifier_events`, whose sends and lookups are rows here
-- now; the windows it held were at most a day long.

CREATE TABLE rate_events (
    principal_id TEXT NOT NULL REFERENCES system_principals(id) ON DELETE CASCADE,
    counter      TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    expires_at   TEXT NOT NULL
) STRICT;

CREATE INDEX rate_events_by_counter ON rate_events (principal_id, counter, created_at);
CREATE INDEX rate_events_by_expiry ON rate_events (principal_id, expires_at);

DROP TABLE agent_verifier_events;
