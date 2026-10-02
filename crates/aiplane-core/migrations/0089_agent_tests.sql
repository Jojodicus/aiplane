-- SPDX-License-Identifier: AGPL-3.0-only
-- Copyright (C) 2026 croit GmbH
--
-- Agent evaluation (issue #99, docs/agents.md §5 "What #99 built").
--
-- A test case is a script (visitor messages, with trusted slot writes between
-- them) plus deterministic expectations and an optional rubric. A run is one
-- execution of every case against the draft or a published version, stored
-- with a result per case.
--
-- 0087 and 0088 are claimed by concurrent branches (#96, #95); this takes
-- 0089. If one of them lands with another number, renumber before merging.
--
-- `spec_hash` and `suite_hash` pin what a run tested: the publish guard only
-- accepts a green run of the draft as it is now against the suite as it is
-- now. `session_id` carries no foreign key: the test conversation is swept by
-- retention like any other, and the stored report outlives it.

CREATE TABLE agent_test_cases (
    id           TEXT PRIMARY KEY NOT NULL,
    principal_id TEXT NOT NULL REFERENCES agents(principal_id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    script       TEXT NOT NULL,
    expect       TEXT NOT NULL,
    rubric       TEXT,
    created_by   TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    UNIQUE (principal_id, name)
) STRICT;

CREATE TABLE agent_test_runs (
    id           TEXT PRIMARY KEY NOT NULL,
    principal_id TEXT NOT NULL REFERENCES agents(principal_id) ON DELETE CASCADE,
    source       TEXT NOT NULL,
    version      INTEGER,
    spec_hash    TEXT NOT NULL,
    suite_hash   TEXT NOT NULL,
    started_by   TEXT NOT NULL,
    started_at   TEXT NOT NULL,
    finished_at  TEXT NOT NULL
) STRICT;

CREATE INDEX idx_agent_test_runs_principal ON agent_test_runs (principal_id, started_at);

CREATE TABLE agent_test_results (
    run_id     TEXT NOT NULL REFERENCES agent_test_runs(id) ON DELETE CASCADE,
    case_id    TEXT NOT NULL,
    case_name  TEXT NOT NULL,
    position   INTEGER NOT NULL,
    passed     INTEGER NOT NULL CHECK (passed IN (0, 1)),
    report     TEXT NOT NULL,
    session_id TEXT,
    PRIMARY KEY (run_id, case_id)
) STRICT;
