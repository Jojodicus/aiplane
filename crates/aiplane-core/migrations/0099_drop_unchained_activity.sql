-- SPDX-License-Identifier: AGPL-3.0-only
-- Copyright (C) 2026 croit GmbH
--
-- Every event of the activity log belongs to a hash chain (docs/agents.md,
-- "Hash chains"). The events written before chains existed have none and
-- cannot be given one after the fact: their hash would vouch for a history
-- nobody signed. No release ever shipped them, so they go, and from here on
-- `verify` reports any event outside a chain as inserted behind the
-- gateway's back.
DELETE FROM agent_audit WHERE chain_key IS NULL;
