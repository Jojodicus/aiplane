-- SPDX-License-Identifier: AGPL-3.0-only
-- Copyright (C) 2026 croit GmbH
--
-- Agent analytics (issue #100, docs/agents.md §5 "What #100 built").
--
-- Analytics is derived from rows that already exist, so this adds an index and
-- no table. The read selects one agent's audit rows by kind inside a time
-- range; `idx_agent_audit_principal` orders by time but cannot skip the many
-- `tool_call` rows between the few kinds the page counts.
--
-- 0085 is left free on purpose: a concurrent branch may claim it.

CREATE INDEX idx_agent_audit_principal_kind
    ON agent_audit (principal_id, kind, created_at);
