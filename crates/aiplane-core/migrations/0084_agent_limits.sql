-- SPDX-License-Identifier: AGPL-3.0-only
-- Copyright (C) 2026 croit GmbH
--
-- Limits, owner budget and retention for public agents (issue #92,
-- docs/agents.md §5 "What #92 built").
--
-- usage_events.agent_id  the main agent at the root of the run's call chain,
--                        NULL outside an agent run. A sub-agent's calls and
--                        the router's classifier call carry the main agent
--                        here, so one read sums what a visitor conversation
--                        cost its owner. `user_id` still names the principal
--                        that made the call.
-- usage_events.chain     the serialized call chain of that run.
--
-- The indexes serve the reads that run on every visitor message: the owner's
-- monthly budget, and the per-IP rate window.

ALTER TABLE usage_events ADD COLUMN agent_id TEXT;
ALTER TABLE usage_events ADD COLUMN chain TEXT;

CREATE INDEX usage_events_agent ON usage_events(agent_id, created_at)
    WHERE agent_id IS NOT NULL;

CREATE INDEX visitor_sessions_ip ON visitor_sessions(principal_id, client_ip);
