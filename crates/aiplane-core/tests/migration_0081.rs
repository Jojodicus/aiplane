// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Migration 0081 rebuilds `chat_sessions` so a conversation can be owned by a
//! system principal instead of a person.
//!
//! `chat_sessions` is the parent of seven tables, all `ON DELETE CASCADE`. A
//! rebuild drops the old table, and with foreign keys on, SQLite's implicit
//! `DELETE` on `DROP TABLE` cascades: every turn, tool call, document and
//! setting of every conversation would go with it, silently. These tests seed
//! a file database as it looked on the previous release, then open it through
//! the real [`aiplane_core::server::db::open`] path — the one production boots
//! through — and check that nothing was lost and every foreign key still holds.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

const LAST_VERSION_BEFORE: i64 = 78;

struct TempDb(PathBuf);

impl TempDb {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("aiplane-0081-{}.sqlite", uuid::Uuid::new_v4())))
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}

/// A file database migrated up to the previous release, connected the way
/// that release connected: foreign keys on.
async fn previous_release(path: &Path) -> SqlitePool {
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .expect("file sqlite");
    let mut migrator = sqlx::migrate!("./migrations");
    migrator.migrations = Cow::Owned(
        migrator
            .migrations
            .iter()
            .filter(|m| m.version <= LAST_VERSION_BEFORE)
            .cloned()
            .collect(),
    );
    migrator
        .run(&pool)
        .await
        .expect("previous release migrates");
    pool
}

const NOW: &str = "2026-09-01T12:00:00Z";

async fn exec(pool: &SqlitePool, sql: &str) {
    sqlx::query(sql)
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

/// Two people, each with a conversation that has a row in every table that
/// hangs off `chat_sessions`.
async fn seed(pool: &SqlitePool) {
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
                "INSERT INTO chat_turn_suspensions (turn_id, request_id, kind, tool_call, tail,
                    budget_used, on_timeout, expires_at, created_at)
                 VALUES ('{turn}', 'r-{user}', 'approval', '{{}}', '[]', '{{}}', 'deny', '{NOW}', '{NOW}')"
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
        ] {
            exec(pool, &sql).await;
        }
    }
}

const CHILDREN: [&str; 10] = [
    "chat_turns",
    "chat_tool_calls",
    "chat_turn_suspensions",
    "chat_turn_steers",
    "chat_session_tools",
    "chat_session_skills",
    "chat_session_settings",
    "documents",
    "chat_compactions",
    "chat_pending_turns",
];

async fn count(pool: &SqlitePool, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(pool)
        .await
        .unwrap()
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
    Vec<(String, i64)>,
    Vec<(String, String, Option<String>, i64, i64)>,
) {
    let db = TempDb::new();
    let before = previous_release(&db.0).await;
    seed(&before).await;
    let mut counts = Vec::new();
    for table in CHILDREN {
        counts.push((table.to_string(), count(&before, table).await));
    }
    let rows = sessions(&before).await;
    before.close().await;

    let after = aiplane_core::server::db::open(&db.0)
        .await
        .expect("the current release opens a previous release's database");
    (db, after, counts, rows)
}

#[tokio::test]
async fn every_session_and_every_row_hanging_off_it_survives() {
    let (_db, pool, counts, rows) = migrated().await;

    assert_eq!(sessions(&pool).await, rows);
    for (table, before) in counts {
        assert!(before > 0, "the seed must cover `{table}`");
        assert_eq!(
            count(&pool, &table).await,
            before,
            "rows lost from `{table}`"
        );
    }
    let dangling: Vec<String> = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&pool)
        .await
        .unwrap()
        .iter()
        .map(|r| r.get::<String, _>(0))
        .collect();
    assert!(dangling.is_empty(), "dangling references in {dangling:?}");
}

#[tokio::test]
async fn migrated_sessions_have_no_principal_owner() {
    let (_db, pool, _, _) = migrated().await;
    let owned_by_principal: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM chat_sessions
          WHERE principal_id IS NOT NULL OR parent_turn_id IS NOT NULL OR agent_version IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(owned_by_principal, 0);
}

#[tokio::test]
async fn children_still_cascade_from_the_rebuilt_table() {
    let (_db, pool, _, _) = migrated().await;
    exec(&pool, "DELETE FROM chat_sessions WHERE id = 's-alice'").await;
    for table in CHILDREN {
        let column = match table {
            "chat_tool_calls" | "chat_turn_suspensions" | "chat_turn_steers" => {
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
    let left: Vec<String> = sqlx::query_scalar("SELECT id FROM chat_sessions")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(left, vec!["s-alice".to_string()]);
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
async fn the_owner_index_is_rebuilt() {
    let (_db, pool, _, _) = migrated().await;
    let indexes: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'chat_sessions'
           AND name NOT LIKE 'sqlite_autoindex%' ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(
        indexes.contains(&"chat_sessions_user_updated".to_string()),
        "{indexes:?}"
    );
    assert!(
        indexes.contains(&"chat_sessions_principal_updated".to_string()),
        "{indexes:?}"
    );
}

#[tokio::test]
async fn a_session_has_exactly_one_owner() {
    let (_db, pool, _, _) = migrated().await;
    exec(
        &pool,
        &format!(
            "INSERT INTO system_principals (id, name, display, created_by, created_at)
             VALUES ('p1', 'support-website', 'Support', 'alice', '{NOW}')"
        ),
    )
    .await;
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
    exec(
        &pool,
        &format!(
            "INSERT INTO system_principals (id, name, display, created_by, created_at)
             VALUES ('p1', 'support-website', 'Support', 'alice', '{NOW}')"
        ),
    )
    .await;
    let res = sqlx::query(&format!(
        "INSERT INTO chat_sessions (id, principal_id, created_at, updated_at, shared)
         VALUES ('s-agent', 'p1', '{NOW}', '{NOW}', 1)"
    ))
    .execute(&pool)
    .await;
    assert!(res.is_err(), "a shared agent conversation was accepted");
}

#[tokio::test]
async fn opening_an_already_migrated_database_again_is_a_no_op() {
    let (db, pool, counts, rows) = migrated().await;
    pool.close().await;
    let again = aiplane_core::server::db::open(&db.0)
        .await
        .expect("second boot");
    assert_eq!(sessions(&again).await, rows);
    for (table, before) in counts {
        assert_eq!(
            count(&again, &table).await,
            before,
            "`{table}` changed on a second boot"
        );
    }
}
