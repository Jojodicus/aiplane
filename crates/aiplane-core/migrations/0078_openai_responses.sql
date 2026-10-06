-- Responses created through `POST /v1/responses` with `store` on, kept so a
-- later request can continue one with `previous_response_id` and its owner can
-- read or delete it. Owned by exactly one person or system principal, and
-- deleted with them; rows past `expires_at` are never served and are pruned.
CREATE TABLE api_responses (
    id                   TEXT PRIMARY KEY NOT NULL,
    user_id              TEXT REFERENCES users(id) ON DELETE CASCADE,
    principal_id         TEXT REFERENCES system_principals(id) ON DELETE CASCADE,
    previous_response_id TEXT,
    input_items          TEXT NOT NULL,
    response             TEXT NOT NULL,
    created_at           INTEGER NOT NULL,
    expires_at           INTEGER NOT NULL,
    CHECK ((user_id IS NULL) <> (principal_id IS NULL))
) STRICT;

CREATE INDEX idx_api_responses_expires ON api_responses (expires_at);
CREATE INDEX idx_api_responses_user ON api_responses (user_id);
CREATE INDEX idx_api_responses_principal ON api_responses (principal_id);
