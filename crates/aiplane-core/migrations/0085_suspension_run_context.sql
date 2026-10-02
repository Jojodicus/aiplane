-- SPDX-License-Identifier: AGPL-3.0-only
-- Copyright (C) 2026 croit GmbH
--
-- Agent runs can suspend (docs/agents.md "Suspend and resume"). A sub-agent
-- run is rebuilt on resume from its session (principal, version) and its
-- parent's suspension (the call chain), but the values its route bound from
-- the caller's state at dispatch time exist nowhere else. They are kept here,
-- next to the pause they belong to, as opaque JSON the dispatcher writes:
--
--   run_context   JSON {route, route_binds}; NULL for every pause that is not
--                 inside a sub-agent run
--
-- A plain ADD COLUMN: nullable, no default, no rows move.

ALTER TABLE chat_turn_suspensions ADD COLUMN run_context TEXT;
