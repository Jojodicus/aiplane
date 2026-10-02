-- Agent definitions (issue #84, design in docs/agents.md §2).
--
-- An agent is a system principal plus a spec, keyed 1:1 by the principal.
-- Its grants stay in `principal_grants` and are not versioned. The spec is
-- edited as `draft_spec`; publishing snapshots the draft as an immutable,
-- numbered `agent_versions` row and points `live_version` at it, so editing
-- a draft can never change what visitors and parent agents run.

CREATE TABLE agents (
    principal_id  TEXT PRIMARY KEY NOT NULL REFERENCES system_principals(id) ON DELETE CASCADE,
    draft_spec    TEXT NOT NULL,
    live_version  INTEGER,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
) STRICT;

CREATE TABLE agent_versions (
    principal_id  TEXT NOT NULL REFERENCES agents(principal_id) ON DELETE CASCADE,
    version       INTEGER NOT NULL,
    spec          TEXT NOT NULL,
    published_by  TEXT NOT NULL,
    published_at  TEXT NOT NULL,
    PRIMARY KEY (principal_id, version)
) STRICT;

-- A share only takes effect for a holder of `can_manage_agents`; that is
-- checked when the share is written and again on every request, not here.
CREATE TABLE agent_shares (
    principal_id  TEXT NOT NULL REFERENCES agents(principal_id) ON DELETE CASCADE,
    subject_kind  TEXT NOT NULL CHECK (subject_kind IN ('user', 'group')),
    subject_id    TEXT NOT NULL,
    access        TEXT NOT NULL CHECK (access IN ('read', 'write')),
    PRIMARY KEY (principal_id, subject_kind, subject_id)
) STRICT;

CREATE INDEX idx_agent_shares_subject ON agent_shares (subject_kind, subject_id);
