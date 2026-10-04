// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Migration 0077 (`0077_agent_builder.sql`) adds the agent builder in one
//! step: it squashes the unreleased chain that built it up piece by piece.
//!
//! Two things can go wrong with that, and each has its tests here:
//!
//! - **The squash is not the same schema.** `fixtures/schema_after_agent_builder.txt`
//!   is the normalized schema the unsquashed chain produced, dumped before
//!   it was deleted, plus what later unpushed work added to 0077 (the agent
//!   architect's tables, #118). A fresh database migrated to 0077 must match
//!   it. While 0077 is unpushed and gets amended, regenerate the fixture with
//!   `UPDATE_SCHEMA_FIXTURE=1` and review the diff: every changed line must be
//!   one the amendment meant.
//! - **The upgrade loses rows.** 0077 rebuilds `chat_sessions`, the parent of
//!   ten tables, all `ON DELETE CASCADE`, and adds columns to
//!   `gateway_groups`, `usage_events` and `mcp_tool_audit`. With foreign keys
//!   on, the rebuild's `DROP TABLE` would cascade through every turn, tool call
//!   and document. These tests seed a file database as the previous release
//!   left it, boot it through the real [`aiplane_core::server::db::open`] path,
//!   and check that nothing was lost and every constraint still holds.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

const LAST_VERSION_BEFORE: i64 = 76;
const AGENT_BUILDER: i64 = 77;
const NOW: &str = "2026-09-01T12:00:00Z";
const SCHEMA_AFTER_AGENT_BUILDER: &str = include_str!("fixtures/schema_after_agent_builder.txt");

struct TempDb(PathBuf);

impl TempDb {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("aiplane-0077-{}.sqlite", uuid::Uuid::new_v4())))
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}

fn migrations_up_to(version: i64) -> Migrator {
    let mut migrator = sqlx::migrate!("./migrations");
    migrator.migrations = Cow::Owned(
        migrator
            .migrations
            .iter()
            .filter(|m| m.version <= version)
            .cloned()
            .collect(),
    );
    migrator
}

async fn file_pool(path: &Path, foreign_keys: bool) -> SqlitePool {
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(foreign_keys);
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .expect("file sqlite")
}

async fn exec(pool: &SqlitePool, sql: &str) {
    sqlx::query(sql)
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

async fn count(pool: &SqlitePool, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn strings(pool: &SqlitePool, sql: &str) -> Vec<String> {
    sqlx::query_scalar(sql)
        .fetch_all(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

// ---------------------------------------------------------------------------
// The squash is the same schema
// ---------------------------------------------------------------------------

/// `sqlite_master.sql` with the insignificant parts of its text removed: runs
/// of whitespace, whitespace next to `(`, `)` and `,` (an `ADD COLUMN` appends
/// `" , col"`, a fresh `CREATE TABLE` writes `", col"`), and the quotes
/// `ALTER TABLE … RENAME` puts around the new table name.
fn normalize(sql: &str) -> String {
    let collapsed = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::with_capacity(collapsed.len());
    let chars: Vec<char> = collapsed.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        let next_is_punct = chars.get(i + 1).is_some_and(|n| "(),".contains(*n));
        let prev_is_punct = out.chars().last().is_some_and(|p| "(),".contains(p));
        if c == ' ' && (next_is_punct || prev_is_punct) {
            continue;
        }
        out.push(c);
    }
    match out.strip_prefix("CREATE TABLE \"") {
        Some(rest) => match rest.split_once('"') {
            Some((name, tail)) => format!("CREATE TABLE {name}{tail}"),
            None => out,
        },
        None => out,
    }
}

/// Every table, index and trigger with its normalized DDL, then each table's
/// columns, foreign keys and indexes as SQLite itself reports them.
async fn schema(pool: &SqlitePool) -> String {
    let mut lines = Vec::new();
    for row in sqlx::query(
        "SELECT type, name, tbl_name, COALESCE(sql, '') AS sql FROM sqlite_master
          WHERE name NOT LIKE '_sqlx%' ORDER BY type, name",
    )
    .fetch_all(pool)
    .await
    .unwrap()
    {
        lines.push(format!(
            "{} {} on {}: {}",
            row.get::<String, _>("type"),
            row.get::<String, _>("name"),
            row.get::<String, _>("tbl_name"),
            normalize(&row.get::<String, _>("sql")),
        ));
    }
    let tables = strings(
        pool,
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE '_sqlx%'
          ORDER BY name",
    )
    .await;
    for table in tables {
        let per_table = [
            (
                "column",
                "SELECT cid || '|' || name || '|' || type || '|' || \"notnull\" || '|'
                        || COALESCE(dflt_value, 'NULL') || '|' || pk || '|' || hidden
                   FROM pragma_table_xinfo(?1)",
            ),
            (
                "fk",
                "SELECT id || '|' || seq || '|' || \"table\" || '|' || \"from\" || '|'
                        || COALESCE(\"to\", 'NULL') || '|' || on_update || '|' || on_delete
                        || '|' || \"match\"
                   FROM pragma_foreign_key_list(?1)",
            ),
        ];
        for (what, sql) in per_table {
            for line in sqlx::query_scalar::<_, String>(sql)
                .bind(&table)
                .fetch_all(pool)
                .await
                .unwrap()
            {
                lines.push(format!("{table} {what} {line}"));
            }
        }
        for index in sqlx::query(
            "SELECT name, \"unique\" || '|' || origin || '|' || partial AS shape
               FROM pragma_index_list(?1) ORDER BY name",
        )
        .bind(&table)
        .fetch_all(pool)
        .await
        .unwrap()
        {
            let name: String = index.get("name");
            lines.push(format!(
                "{table} index {name} {}",
                index.get::<String, _>("shape")
            ));
            for column in sqlx::query_scalar::<_, String>(
                "SELECT seqno || '|' || cid || '|' || COALESCE(name, 'NULL') || '|' || \"desc\"
                        || '|' || coll || '|' || \"key\"
                   FROM pragma_index_xinfo(?1)",
            )
            .bind(&name)
            .fetch_all(pool)
            .await
            .unwrap()
            {
                lines.push(format!("{table} index {name} {column}"));
            }
        }
    }
    lines.join("\n") + "\n"
}

#[tokio::test]
async fn a_fresh_database_gets_the_schema_the_unsquashed_chain_produced() {
    let db = TempDb::new();
    let pool = file_pool(&db.0, false).await;
    migrations_up_to(AGENT_BUILDER)
        .run(&pool)
        .await
        .expect("migrates to 0077");
    let actual = schema(&pool).await;
    if std::env::var_os("UPDATE_SCHEMA_FIXTURE").is_some() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/schema_after_agent_builder.txt");
        std::fs::write(&path, &actual).expect("writes the schema fixture");
        return;
    }
    if actual != SCHEMA_AFTER_AGENT_BUILDER {
        let first_difference = actual
            .lines()
            .zip(SCHEMA_AFTER_AGENT_BUILDER.lines())
            .find(|(a, f)| a != f);
        panic!(
            "0077 no longer produces the schema of the chain it replaced \
             (tests/fixtures/schema_after_agent_builder.txt). Either an earlier migration or \
             the dump changed, or an amendment to the unpushed 0077 needs the fixture \
             regenerated (UPDATE_SCHEMA_FIXTURE=1). First difference (actual, fixture): \
             {first_difference:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The upgrade from the previous release is lossless
// ---------------------------------------------------------------------------

/// The tables of the previous release that 0077 rebuilds or alters, and the
/// ones that hang off `chat_sessions`.
const SEEDED: [&str; 13] = [
    "chat_sessions",
    "chat_turns",
    "chat_tool_calls",
    "chat_turn_steers",
    "chat_session_tools",
    "chat_session_skills",
    "chat_session_settings",
    "documents",
    "chat_compactions",
    "chat_pending_turns",
    "gateway_groups",
    "usage_events",
    "mcp_tool_audit",
];

const SESSION_CHILDREN: [&str; 9] = [
    "chat_turns",
    "chat_tool_calls",
    "chat_turn_steers",
    "chat_session_tools",
    "chat_session_skills",
    "chat_session_settings",
    "documents",
    "chat_compactions",
    "chat_pending_turns",
];

async fn previous_release(path: &Path) -> SqlitePool {
    let pool = file_pool(path, true).await;
    migrations_up_to(LAST_VERSION_BEFORE)
        .run(&pool)
        .await
        .expect("previous release migrates");
    pool
}

/// Two people, each with a conversation that has a row in every table that
/// hangs off `chat_sessions`, a usage event and an MCP audit row; and two
/// gateway groups.
async fn seed(pool: &SqlitePool) {
    for (group, admin) in [("staff", 1), ("everyone", 0)] {
        exec(
            pool,
            &format!(
                "INSERT INTO gateway_groups (name, description, is_admin, is_default, created_at, updated_at)
                 VALUES ('{group}', 'the {group}', {admin}, {}, '{NOW}', '{NOW}')",
                1 - admin
            ),
        )
        .await;
    }
    for user in ["alice", "bob"] {
        exec(
            pool,
            &format!(
                "INSERT INTO users (id, email, created_at, updated_at)
                 VALUES ('{user}', '{user}@example.com', '{NOW}', '{NOW}')"
            ),
        )
        .await;
        let s = format!("s-{user}");
        exec(
            pool,
            &format!(
                "INSERT INTO chat_sessions (id, user_id, title, created_at, updated_at, shared, pinned)
                 VALUES ('{s}', '{user}', 'Plans of {user}', '{NOW}', '{NOW}', 1, 1)"
            ),
        )
        .await;
        for (seq, role, body) in [(0, "user", "find the zebra"), (1, "assistant", "found it")] {
            let column = if role == "user" {
                "user_content"
            } else {
                "content"
            };
            exec(
                pool,
                &format!(
                    "INSERT INTO chat_turns (id, session_id, seq, role, {column}, status, created_at)
                     VALUES ('t{seq}-{user}', '{s}', {seq}, '{role}', '{body} {user}', 'completed', '{NOW}')"
                ),
            )
            .await;
        }
        let turn = format!("t1-{user}");
        for sql in [
            format!(
                "INSERT INTO chat_tool_calls (id, turn_id, seq, name, arguments_json, status, created_at)
                 VALUES ('c1', '{turn}', 0, 'echo', '{{}}', 'completed', '{NOW}')"
            ),
            format!(
                "INSERT INTO chat_turn_steers (id, turn_id, seq, text, created_at)
                 VALUES ('st-{user}', '{turn}', 0, 'faster', '{NOW}')"
            ),
            format!(
                "INSERT INTO chat_session_tools (session_id, tool_key, enabled, source, updated_at)
                 VALUES ('{s}', 'web', 1, 'user', '{NOW}')"
            ),
            format!(
                "INSERT INTO chat_session_skills (session_id, skill_name, loaded_at)
                 VALUES ('{s}', 'letters', '{NOW}')"
            ),
            format!(
                "INSERT INTO chat_session_settings (session_id, effort, updated_at)
                 VALUES ('{s}', 'high', '{NOW}')"
            ),
            format!(
                "INSERT INTO documents (id, session_id, user_id, title, format, current_ver,
                    created_at, updated_at)
                 VALUES ('doc-{user}', '{s}', '{user}', 'Draft', 'markdown', 1, '{NOW}', '{NOW}')"
            ),
            format!(
                "INSERT INTO chat_compactions (session_id, up_to_seq, summary, created_at, updated_at)
                 VALUES ('{s}', 0, 'earlier', '{NOW}', '{NOW}')"
            ),
            format!(
                "INSERT INTO chat_pending_turns (turn_id, session_id, user_id, model, created_at)
                 VALUES ('t0-{user}', '{s}', '{user}', 'm', '{NOW}')"
            ),
            format!(
                "INSERT INTO usage_events
                   (id, created_at, user_id, user_email, token_id, token_name, source, kind,
                    backend, model, status, duration_ms, prompt_tokens, completion_tokens,
                    total_tokens, cost, enforce_limits, input_units, output_units)
                 VALUES ('u-{user}', '{NOW}', '{user}', '{user}@example.com', NULL, NULL, 'chat',
                         'chat', 'b', 'qwen', 200, 5, 1, 2, 3, 0.5, 1, 1, 2)"
            ),
            format!(
                "INSERT INTO mcp_tool_audit (id, user_id, user_email, connector_key, tool_id,
                    arguments, outcome, error, session_id, created_at)
                 VALUES ('a-{user}', '{user}', '{user}@example.com', 'jira', 'mcp__jira__search',
                         '{{}}', 'ok', NULL, '{s}', '{NOW}')"
            ),
        ] {
            exec(pool, &sql).await;
        }
    }
}

type Snapshot = Vec<(&'static str, i64)>;

async fn snapshot(pool: &SqlitePool) -> Snapshot {
    let mut counts = Vec::new();
    for table in SEEDED {
        counts.push((table, count(pool, table).await));
    }
    counts
}

async fn sessions(pool: &SqlitePool) -> Vec<(String, String, Option<String>, i64, i64)> {
    sqlx::query("SELECT id, user_id, title, shared, pinned FROM chat_sessions ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
        .iter()
        .map(|r| {
            (
                r.get("id"),
                r.get("user_id"),
                r.get("title"),
                r.get("shared"),
                r.get("pinned"),
            )
        })
        .collect()
}

/// Seed the previous release, then boot the current one on the same file.
async fn migrated() -> (
    TempDb,
    SqlitePool,
    Snapshot,
    Vec<(String, String, Option<String>, i64, i64)>,
) {
    let db = TempDb::new();
    let before = previous_release(&db.0).await;
    seed(&before).await;
    let counts = snapshot(&before).await;
    let rows = sessions(&before).await;
    before.close().await;

    let after = aiplane_core::server::db::open(&db.0)
        .await
        .expect("the current release opens a previous release's database");
    (db, after, counts, rows)
}

async fn add_principal(pool: &SqlitePool, id: &str) {
    exec(
        pool,
        &format!(
            "INSERT INTO system_principals (id, name, display, created_by, created_at)
             VALUES ('{id}', '{id}', '{id}', 'alice', '{NOW}')"
        ),
    )
    .await;
}

#[tokio::test]
async fn every_row_of_the_previous_release_survives() {
    let (_db, pool, counts, rows) = migrated().await;

    assert_eq!(sessions(&pool).await, rows);
    for (table, before) in counts {
        assert!(before > 0, "the seed must cover `{table}`");
        assert_eq!(
            count(&pool, table).await,
            before,
            "rows lost from `{table}`"
        );
    }
    let dangling = strings(&pool, "SELECT \"table\" FROM pragma_foreign_key_check").await;
    assert!(dangling.is_empty(), "dangling references in {dangling:?}");
}

#[tokio::test]
async fn existing_rows_get_the_defaults_of_the_new_columns() {
    let (_db, pool, _, _) = migrated().await;
    let person_owned: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM chat_sessions
          WHERE user_id IS NOT NULL AND principal_id IS NULL AND parent_turn_id IS NULL
            AND agent_version IS NULL AND visitor_id IS NULL AND lang IS NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(person_owned, 2);
    assert_eq!(
        strings(
            &pool,
            "SELECT DISTINCT principal_kind || '|' || COALESCE(agent_id, 'NULL') || '|'
                    || COALESCE(chain, 'NULL') || '|' || total_tokens || '|' || cost
               FROM usage_events"
        )
        .await,
        vec!["user|NULL|NULL|3|0.5".to_string()]
    );
    assert_eq!(
        strings(
            &pool,
            "SELECT DISTINCT principal_kind || '|' || COALESCE(chain, 'NULL') || '|' || tool_id
               FROM mcp_tool_audit"
        )
        .await,
        vec!["user|NULL|mcp__jira__search".to_string()]
    );
    assert_eq!(
        strings(
            &pool,
            "SELECT name || '|' || is_admin || '|' || can_manage_agents FROM gateway_groups
              ORDER BY name"
        )
        .await,
        vec!["everyone|0|0".to_string(), "staff|1|0".to_string()]
    );
}

#[tokio::test]
async fn children_still_cascade_from_the_rebuilt_sessions_table() {
    let (_db, pool, _, _) = migrated().await;
    exec(&pool, "DELETE FROM chat_sessions WHERE id = 's-alice'").await;
    for table in SESSION_CHILDREN {
        let column = match table {
            "chat_tool_calls" | "chat_turn_steers" => {
                "(SELECT session_id FROM chat_turns WHERE chat_turns.id = turn_id)"
            }
            _ => "session_id",
        };
        let left: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*) FROM {table} WHERE {column} = 's-alice' OR {column} IS NULL"
        ))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(left, 0, "`{table}` kept rows of a deleted session");
    }
    assert_eq!(
        count(&pool, "chat_turns").await,
        2,
        "bob's turns are untouched"
    );
}

#[tokio::test]
async fn deleting_a_person_still_deletes_their_sessions() {
    let (_db, pool, _, _) = migrated().await;
    exec(&pool, "DELETE FROM users WHERE id = 'bob'").await;
    assert_eq!(
        strings(&pool, "SELECT id FROM chat_sessions").await,
        vec!["s-alice".to_string()]
    );
}

#[tokio::test]
async fn full_text_search_still_finds_migrated_turns() {
    let (_db, pool, _, _) = migrated().await;
    let hits = session_core::db::search_sessions(&pool, "alice", "zebra", 10)
        .await
        .unwrap();
    assert_eq!(
        hits.iter()
            .map(|h| h.session_id.as_str())
            .collect::<Vec<_>>(),
        vec!["s-alice"]
    );
}

#[tokio::test]
async fn the_sessions_indexes_are_rebuilt() {
    let (_db, pool, _, _) = migrated().await;
    let indexes = strings(
        &pool,
        "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'chat_sessions'
           AND name NOT LIKE 'sqlite_autoindex%' ORDER BY name",
    )
    .await;
    assert_eq!(
        indexes,
        [
            "chat_sessions_parent_turn",
            "chat_sessions_principal_updated",
            "chat_sessions_user_updated"
        ]
    );
}

#[tokio::test]
async fn a_session_has_exactly_one_owner() {
    let (_db, pool, _, _) = migrated().await;
    add_principal(&pool, "p1").await;
    for (owner, ok) in [
        ("'alice', NULL", true),
        ("NULL, 'p1'", true),
        ("NULL, NULL", false),
        ("'alice', 'p1'", false),
    ] {
        let id = uuid::Uuid::new_v4();
        let res = sqlx::query(&format!(
            "INSERT INTO chat_sessions (id, user_id, principal_id, created_at, updated_at)
             VALUES ('{id}', {owner}, '{NOW}', '{NOW}')"
        ))
        .execute(&pool)
        .await;
        assert_eq!(res.is_ok(), ok, "owner ({owner}): {res:?}");
    }
}

#[tokio::test]
async fn an_agent_conversation_can_never_be_shared() {
    let (_db, pool, _, _) = migrated().await;
    add_principal(&pool, "p1").await;
    let res = sqlx::query(&format!(
        "INSERT INTO chat_sessions (id, principal_id, created_at, updated_at, shared)
         VALUES ('s-agent', 'p1', '{NOW}', '{NOW}', 1)"
    ))
    .execute(&pool)
    .await;
    assert!(res.is_err(), "a shared agent conversation was accepted");
}

#[tokio::test]
async fn deleting_a_principal_deletes_its_conversations_and_their_suspensions() {
    let (_db, pool, _, _) = migrated().await;
    add_principal(&pool, "p1").await;
    for sql in [
        format!(
            "INSERT INTO chat_sessions (id, principal_id, created_at, updated_at)
             VALUES ('s-agent', 'p1', '{NOW}', '{NOW}')"
        ),
        format!(
            "INSERT INTO chat_turns (id, session_id, seq, role, content, status, created_at)
             VALUES ('t-agent', 's-agent', 0, 'assistant', 'waiting', 'suspended', '{NOW}')"
        ),
        format!(
            "INSERT INTO chat_turn_suspensions (turn_id, request_id, kind, tool_call, tail,
                budget_used, on_timeout, expires_at, created_at)
             VALUES ('t-agent', 'r1', 'approval', '{{}}', '[]', '{{}}', 'deny', '{NOW}', '{NOW}')"
        ),
    ] {
        exec(&pool, &sql).await;
    }
    exec(&pool, "DELETE FROM system_principals WHERE id = 'p1'").await;
    assert_eq!(count(&pool, "chat_turn_suspensions").await, 0);
    assert_eq!(
        strings(&pool, "SELECT id FROM chat_sessions ORDER BY id").await,
        vec!["s-alice".to_string(), "s-bob".to_string()]
    );
}

#[tokio::test]
async fn opening_an_already_migrated_database_again_is_a_no_op() {
    let (db, pool, counts, rows) = migrated().await;
    let schema_before = schema(&pool).await;
    pool.close().await;
    let again = aiplane_core::server::db::open(&db.0)
        .await
        .expect("second boot");
    assert_eq!(sessions(&again).await, rows);
    for (table, before) in counts {
        assert_eq!(
            count(&again, table).await,
            before,
            "`{table}` changed on a second boot"
        );
    }
    assert_eq!(schema(&again).await, schema_before);
}

// ---------------------------------------------------------------------------
// Constraints and indexes of the new tables
// ---------------------------------------------------------------------------

async fn fresh() -> SqlitePool {
    aiplane_core::server::db::open(Path::new(":memory:"))
        .await
        .unwrap()
}

async fn grant(pool: &SqlitePool, principal: &str, kind: &str, r: &str) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO principal_grants (principal_id, kind, ref, granted_by, granted_at)
         VALUES (?, ?, ?, 'alice', ?)",
    )
    .bind(principal)
    .bind(kind)
    .bind(r)
    .bind(NOW)
    .execute(pool)
    .await
    .map(drop)
}

#[tokio::test]
async fn a_grant_has_a_known_kind() {
    let pool = fresh().await;
    add_principal(&pool, "ci").await;
    for (kind, r) in [
        ("tool", "time"),
        ("connector", "jira"),
        ("skill", "letters"),
        ("rag_collection", "7"),
        ("model", "qwen"),
        ("a2a_caller", "support"),
        (
            "a2a_agent",
            "https://partner.example.com/.well-known/agent-card.json",
        ),
    ] {
        grant(&pool, "ci", kind, r)
            .await
            .unwrap_or_else(|e| panic!("{kind}: {e}"));
    }
    assert!(
        grant(&pool, "ci", "pool", "chat").await.is_err(),
        "the CHECK holds"
    );
}

#[tokio::test]
async fn grants_cascade_from_their_principal() {
    let pool = fresh().await;
    for p in ["ci", "support"] {
        add_principal(&pool, p).await;
        grant(&pool, p, "model", "qwen").await.unwrap();
    }
    exec(&pool, "DELETE FROM system_principals WHERE id = 'ci'").await;
    assert_eq!(
        strings(&pool, "SELECT DISTINCT principal_id FROM principal_grants").await,
        vec!["support".to_string()]
    );
}

/// Who answers an agent's inbox is a share level, not a second table.
#[tokio::test]
async fn a_share_is_respond_read_or_write() {
    let pool = fresh().await;
    add_principal(&pool, "support").await;
    exec(
        &pool,
        &format!(
            "INSERT INTO agents (principal_id, draft_spec, created_at, updated_at)
             VALUES ('support', '{{}}', '{NOW}', '{NOW}')"
        ),
    )
    .await;
    let share = |subject: &'static str, access: &'static str| {
        sqlx::query(
            "INSERT INTO agent_shares (principal_id, subject_kind, subject_id, access)
             VALUES ('support', 'user', ?, ?)",
        )
        .bind(subject)
        .bind(access)
        .execute(&pool)
    };
    for (subject, access) in [("sam", "respond"), ("kim", "read"), ("ada", "write")] {
        share(subject, access)
            .await
            .unwrap_or_else(|e| panic!("{access}: {e}"));
    }
    assert!(share("nora", "admin").await.is_err(), "the CHECK holds");
    assert!(
        strings(
            &pool,
            "SELECT name FROM sqlite_master WHERE name = 'agent_responders'"
        )
        .await
        .is_empty()
    );
}

async fn plan(pool: &SqlitePool, sql: &str) -> String {
    sqlx::query_as::<_, (i64, i64, i64, String)>(&format!("EXPLAIN QUERY PLAN {sql}"))
        .bind("x")
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .map(|(_, _, _, detail)| detail)
        .collect::<Vec<_>>()
        .join("; ")
}

#[tokio::test]
async fn the_expiry_sweep_searches_the_deadline_index() {
    let pool = fresh().await;
    let plan = plan(
        &pool,
        "SELECT turn_id FROM chat_turn_suspensions WHERE expires_at < ?",
    )
    .await;
    assert!(plan.contains("chat_turn_suspensions_expires_at"), "{plan}");
}

#[tokio::test]
async fn a_visitor_session_is_found_by_its_chat_session_through_the_index() {
    let pool = fresh().await;
    let plan = plan(
        &pool,
        "SELECT id FROM visitor_sessions WHERE session_id = ?",
    )
    .await;
    assert!(plan.contains("visitor_sessions_session"), "{plan}");
}
