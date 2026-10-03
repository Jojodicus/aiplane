-- The agent architect (#118).

-- A person's chat that runs as the architect: its system prompt and its tools
-- instead of the person's chat tools. `agent_id` is the agent it plans, NULL
-- until it creates one.
CREATE TABLE agent_architect_sessions (
    session_id  TEXT PRIMARY KEY NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
    user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    agent_id    TEXT REFERENCES agents(principal_id) ON DELETE SET NULL,
    created_at  TEXT NOT NULL
) STRICT;

CREATE INDEX agent_architect_sessions_by_user
    ON agent_architect_sessions (user_id, agent_id, created_at);

-- The draft as it was before each save, so a change can be undone. Only the
-- newest few per agent are kept.
CREATE TABLE agent_draft_revisions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    principal_id TEXT NOT NULL REFERENCES agents(principal_id) ON DELETE CASCADE,
    spec        TEXT NOT NULL,
    saved_by    TEXT NOT NULL,
    saved_at    TEXT NOT NULL
) STRICT;

CREATE INDEX agent_draft_revisions_by_agent ON agent_draft_revisions (principal_id, id);
