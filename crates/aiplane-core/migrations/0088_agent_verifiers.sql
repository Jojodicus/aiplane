-- SPDX-License-Identifier: AGPL-3.0-only
-- Copyright (C) 2026 croit GmbH
--
-- What the gateway keeps to run identity verifiers (issue #95,
-- docs/agents.md "What #95 built"). The code itself is stored nowhere: the
-- customer's connector generates, sends and checks it. The gateway owns the
-- counting around it.
--
-- `agent_verifier_codes`: the one outstanding code of a verifier in a
-- conversation — when it was sent, when it stops being accepted, and how
-- many checks it has had. `email_hash` (SHA-256 of the normalised address)
-- ties a check to the address the code went to, so changing the email slot
-- between sending and checking cannot redirect a code.
--
-- `agent_verifier_events`: every code sent and every lookup made, the rows
-- the sliding-window rate limits count (per address, per client IP, per
-- conversation). Address and IP are hashed: the windows need equality, not
-- the values.
--
-- `agent_identity_jtis`: host identity tokens already accepted, by `jti`,
-- so one token cannot be presented twice. Kept until the token expires.

CREATE TABLE agent_verifier_codes (
    session_id  TEXT NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
    verifier    TEXT NOT NULL,
    email_hash  TEXT NOT NULL,
    sent_at     TEXT NOT NULL,
    expires_at  TEXT NOT NULL,
    attempts    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (session_id, verifier)
) STRICT;

CREATE TABLE agent_verifier_events (
    id           TEXT PRIMARY KEY NOT NULL,
    principal_id TEXT NOT NULL REFERENCES system_principals(id) ON DELETE CASCADE,
    verifier     TEXT NOT NULL,
    kind         TEXT NOT NULL CHECK (kind IN ('send', 'lookup')),
    session_id   TEXT NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
    email_hash   TEXT,
    ip_hash      TEXT,
    created_at   TEXT NOT NULL
) STRICT;

CREATE INDEX agent_verifier_events_by_email
    ON agent_verifier_events (principal_id, verifier, email_hash, created_at);
CREATE INDEX agent_verifier_events_by_ip
    ON agent_verifier_events (principal_id, verifier, ip_hash, created_at);
CREATE INDEX agent_verifier_events_by_session
    ON agent_verifier_events (session_id, verifier, created_at);

CREATE TABLE agent_identity_jtis (
    principal_id TEXT NOT NULL REFERENCES system_principals(id) ON DELETE CASCADE,
    jti_hash     TEXT NOT NULL,
    expires_at   TEXT NOT NULL,
    PRIMARY KEY (principal_id, jti_hash)
) STRICT;
