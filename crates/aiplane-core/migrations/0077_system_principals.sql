-- Non-person principals (issue #77, design in docs/agents.md §1).
--
-- A system principal is an identity that is not a user: CI, an integration,
-- and later every agent. It lives in its own table rather than as a `kind`
-- on `users` so that none of the person paths (default groups, empty
-- `allowed_groups` meaning "everyone", per-user MCP, memory, the OIDC upsert)
-- can ever see one by accident: the compiler finds every site instead.
--
-- Default deny: a principal holds exactly the rows in `principal_grants` and
-- nothing else. A grant is written by a user holding `can_manage_agents`,
-- capped at what that user held at the time, and it is never re-derived
-- from that user's rights afterwards — `granted_by` is audit only, not a
-- foreign key, so a manager leaving does not take the grant with them.

CREATE TABLE system_principals (
    id          TEXT PRIMARY KEY NOT NULL,
    name        TEXT NOT NULL UNIQUE,
    display     TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    created_by  TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    disabled_at TEXT
) STRICT;

CREATE TABLE principal_grants (
    principal_id TEXT NOT NULL REFERENCES system_principals(id) ON DELETE CASCADE,
    kind         TEXT NOT NULL CHECK (kind IN ('tool', 'connector', 'skill', 'rag_collection', 'pool')),
    ref          TEXT NOT NULL,
    granted_by   TEXT NOT NULL,
    granted_at   TEXT NOT NULL,
    PRIMARY KEY (principal_id, kind, ref)
) STRICT;

-- Separate from `tokens` with its own `gws_` prefix: `require_bearer` routes
-- by prefix, so a user-token lookup can never return a principal and the
-- other way round.
CREATE TABLE system_tokens (
    id           TEXT PRIMARY KEY NOT NULL,
    principal_id TEXT NOT NULL REFERENCES system_principals(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    hash         TEXT NOT NULL UNIQUE,
    created_by   TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    last_used_at TEXT,
    expires_at   TEXT NOT NULL,
    revoked_at   TEXT
) STRICT;

CREATE INDEX idx_system_tokens_principal ON system_tokens (principal_id);

-- Like `is_admin`: a capability on a gateway group. `is_admin` implies it.
ALTER TABLE gateway_groups ADD COLUMN can_manage_agents INTEGER NOT NULL DEFAULT 0;

-- Who is billed: `user_id` keeps holding the subject (users.id or
-- system_principals.id); this says which of the two it is, so every existing
-- per-user query stays correct.
ALTER TABLE usage_events ADD COLUMN principal_kind TEXT NOT NULL DEFAULT 'user';
ALTER TABLE mcp_tool_audit ADD COLUMN principal_kind TEXT NOT NULL DEFAULT 'user';

-- Append-only trail of everything done to or by a principal. #77 writes the
-- management events (principal created/disabled, grant added/removed, token
-- issued/revoked); later issues add run events with a serialized `chain`.
-- No foreign keys on purpose: the trail must outlive the principal and the
-- acting user, and must not be erasable by a cascade.
CREATE TABLE agent_audit (
    id           TEXT PRIMARY KEY NOT NULL,
    kind         TEXT NOT NULL,
    principal_id TEXT NOT NULL,
    actor_id     TEXT,
    chain        TEXT,
    detail       TEXT NOT NULL,
    created_at   TEXT NOT NULL
) STRICT;

CREATE INDEX idx_agent_audit_principal ON agent_audit (principal_id, created_at DESC);
