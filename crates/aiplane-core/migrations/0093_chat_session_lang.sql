-- SPDX-License-Identifier: AGPL-3.0-only
-- Copyright (C) 2026 croit GmbH
--
-- The visitor's language on an agent conversation (docs/agents.md "Suspend
-- and resume"). Written whenever a turn of the conversation starts, from the
-- language that turn was asked in (the visitor's Accept-Language on the embed
-- and A2A endpoints). A resume reads it, so a paused run continues in the
-- visitor's language whoever answers the pause: staff in another language,
-- or nobody (the timeout sweeper). NULL on a person's chat and on a
-- conversation that has not started a turn since this migration.

ALTER TABLE chat_sessions ADD COLUMN lang TEXT;
