-- SPDX-License-Identifier: AGPL-3.0-only
-- Copyright (C) 2026 croit GmbH
--
-- Public agent endpoint: embed keys and visitor sessions (issue #91,
-- docs/agents.md §5 "Visitor sessions and embedding").
--
-- agent_embed_keys  one per website that may embed an agent. The key
--                   (`gwe_…`) ships in that site's page source, so it is not
--                   a secret, but it is stored as SHA-256 like every other
--                   credential: the plaintext is shown once, on creation.
--                   `origins` is a JSON array of exact `scheme://host[:port]`.
-- visitor_sessions  one per visitor conversation. The visitor token
--                   (`gwv_…`) lives in the host page's sessionStorage and is
--                   stored here as SHA-256. `expires_at` slides by
--                   `idle_ttl_secs` on every request and never passes
--                   `max_expires_at`, the absolute cap.
--
-- `chat_sessions.visitor_id` links a principal-owned conversation to the
-- visitor it serves. A plain ADD COLUMN is enough: the new column has a NULL
-- default, so SQLite accepts its REFERENCES clause without a table rebuild.

CREATE TABLE agent_embed_keys (
    id           TEXT PRIMARY KEY NOT NULL,
    principal_id TEXT NOT NULL REFERENCES agents(principal_id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    key_hash     TEXT NOT NULL UNIQUE,
    origins      TEXT NOT NULL,
    created_by   TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    revoked_at   TEXT
) STRICT;

CREATE INDEX agent_embed_keys_principal ON agent_embed_keys(principal_id);

CREATE TABLE visitor_sessions (
    id             TEXT PRIMARY KEY NOT NULL,
    principal_id   TEXT NOT NULL REFERENCES agents(principal_id) ON DELETE CASCADE,
    embed_key_id   TEXT NOT NULL REFERENCES agent_embed_keys(id) ON DELETE CASCADE,
    token_hash     TEXT NOT NULL UNIQUE,
    session_id     TEXT NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
    client_ip      TEXT,
    idle_ttl_secs  INTEGER NOT NULL CHECK (idle_ttl_secs > 0),
    created_at     TEXT NOT NULL,
    last_seen_at   TEXT NOT NULL,
    expires_at     TEXT NOT NULL,
    max_expires_at TEXT NOT NULL
) STRICT;

CREATE INDEX visitor_sessions_principal ON visitor_sessions(principal_id);

ALTER TABLE chat_sessions ADD COLUMN visitor_id TEXT
    REFERENCES visitor_sessions(id) ON DELETE SET NULL;
