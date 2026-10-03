-- SPDX-License-Identifier: AGPL-3.0-only
-- Copyright (C) 2026 croit GmbH
--
-- How far each chain of the activity log was last found sound
-- (docs/agents.md, "Verification watermarks"), so a check resumes there
-- instead of re-hashing the whole log. `book` is the agent chain's anchors
-- and sweep markers as of `seq`. `mac` signs chain_key, seq, hash and book
-- under the log key `key_id` names, so the database alone cannot move a
-- watermark forward over a change. The retention sweep deletes a
-- conversation chain's row with the chain.
CREATE TABLE activity_verified (
    chain_key   TEXT PRIMARY KEY NOT NULL,
    seq         INTEGER NOT NULL,
    hash        TEXT,
    book        TEXT,
    key_id      TEXT NOT NULL,
    mac         TEXT NOT NULL,
    verified_at TEXT NOT NULL
) STRICT;
