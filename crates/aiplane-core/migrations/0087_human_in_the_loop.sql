-- SPDX-License-Identifier: AGPL-3.0-only
-- Copyright (C) 2026 croit GmbH
--
-- Human in the loop (issue #96, docs/agents.md "What #96 built").
--
-- chat_turn_suspensions.notified_at
--                  when the "a turn is waiting" notification went out for
--                  this pause. Set once with `WHERE notified_at IS NULL`, so
--                  a pause notifies exactly once however often it is looked
--                  at. A new pause is a new row, and notifies again.
-- agent_responders users and groups who may answer an agent's approvals and
--                  handoffs in the inbox without a share: they see the
--                  pending item and its minimal context, never the spec or
--                  the agent's conversations. Unlike a share, a responder
--                  needs no agent-management permission.
-- agent_notify_channels
--                  where an agent's waiting turns are announced besides Web
--                  Push: a Slack or Discord incoming-webhook URL, sealed at
--                  rest (the URL is the credential). `details` = 1 also puts
--                  the question or the tool name into the message; off, it
--                  carries only the agent, the kind and the inbox link.
--
-- New tables and a nullable ADD COLUMN: no rows move.

ALTER TABLE chat_turn_suspensions ADD COLUMN notified_at TEXT;

CREATE TABLE agent_responders (
    principal_id  TEXT NOT NULL REFERENCES agents(principal_id) ON DELETE CASCADE,
    subject_kind  TEXT NOT NULL CHECK (subject_kind IN ('user', 'group')),
    subject_id    TEXT NOT NULL,
    added_by      TEXT NOT NULL,
    added_at      TEXT NOT NULL,
    PRIMARY KEY (principal_id, subject_kind, subject_id)
) STRICT;

CREATE TABLE agent_notify_channels (
    id            TEXT PRIMARY KEY NOT NULL,
    principal_id  TEXT NOT NULL REFERENCES agents(principal_id) ON DELETE CASCADE,
    kind          TEXT NOT NULL CHECK (kind IN ('slack', 'discord')),
    name          TEXT NOT NULL,
    url_nonce     BLOB NOT NULL,
    url_ct        BLOB NOT NULL,
    url_host      TEXT NOT NULL,
    details       INTEGER NOT NULL DEFAULT 0 CHECK (details IN (0, 1)),
    lang          TEXT NOT NULL DEFAULT 'en',
    created_by    TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    UNIQUE (principal_id, name)
) STRICT;

CREATE INDEX agent_notify_channels_principal ON agent_notify_channels(principal_id);
