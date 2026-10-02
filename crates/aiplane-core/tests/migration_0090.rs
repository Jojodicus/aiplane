// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Migration 0090 rebuilds `principal_grants` to admit the `a2a_caller` grant
//! kind. A rebuild that drops a principal's grants would silently take rights
//! away (or, worse, leave an agent half-configured), so this seeds a file
//! database as the previous release left it and boots the current one on it.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

const LAST_VERSION_BEFORE: i64 = 89;
const NOW: &str = "2026-09-01T12:00:00Z";

struct TempDb(PathBuf);

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}

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

async fn exec(pool: &SqlitePool, sql: &str) {
    sqlx::query(sql)
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

async fn grants(pool: &SqlitePool) -> Vec<(String, String, String, String, String)> {
    sqlx::query(
        "SELECT principal_id, kind, ref, granted_by, granted_at FROM principal_grants
          ORDER BY principal_id, kind, ref",
    )
    .fetch_all(pool)
    .await
    .unwrap()
    .iter()
    .map(|r| {
        (
            r.get("principal_id"),
            r.get("kind"),
            r.get("ref"),
            r.get("granted_by"),
            r.get("granted_at"),
        )
    })
    .collect()
}

async fn migrated() -> (
    TempDb,
    SqlitePool,
    Vec<(String, String, String, String, String)>,
) {
    let db =
        TempDb(std::env::temp_dir().join(format!("aiplane-0090-{}.sqlite", uuid::Uuid::new_v4())));
    let before = previous_release(&db.0).await;
    for p in ["ci", "support"] {
        exec(
            &before,
            &format!(
                "INSERT INTO system_principals (id, name, display, created_by, created_at)
                 VALUES ('{p}', '{p}', '{p}', 'root', '{NOW}')"
            ),
        )
        .await;
        for (kind, r) in [("pool", "chat"), ("tool", "time"), ("rag_collection", "7")] {
            exec(
                &before,
                &format!(
                    "INSERT INTO principal_grants (principal_id, kind, ref, granted_by, granted_at)
                     VALUES ('{p}', '{kind}', '{r}', 'alice', '{NOW}')"
                ),
            )
            .await;
        }
    }
    let seeded = grants(&before).await;
    before.close().await;
    let after = aiplane_core::server::db::open(&db.0)
        .await
        .expect("the current release opens a previous release's database");
    (db, after, seeded)
}

#[tokio::test]
async fn every_grant_survives_the_rebuild() {
    let (_db, pool, seeded) = migrated().await;
    assert_eq!(seeded.len(), 6);
    assert_eq!(grants(&pool).await, seeded);
}

#[tokio::test]
async fn the_rebuilt_table_admits_an_a2a_caller_grant_and_nothing_unknown() {
    let (_db, pool, _) = migrated().await;
    exec(
        &pool,
        &format!(
            "INSERT INTO principal_grants (principal_id, kind, ref, granted_by, granted_at)
             VALUES ('ci', 'a2a_caller', 'support', 'alice', '{NOW}')"
        ),
    )
    .await;
    let refused = sqlx::query(&format!(
        "INSERT INTO principal_grants (principal_id, kind, ref, granted_by, granted_at)
         VALUES ('ci', 'model', 'x', 'alice', '{NOW}')"
    ))
    .execute(&pool)
    .await;
    assert!(refused.is_err(), "the CHECK still holds");
}

#[tokio::test]
async fn grants_still_cascade_from_their_principal() {
    let (_db, pool, _) = migrated().await;
    exec(&pool, "DELETE FROM system_principals WHERE id = 'ci'").await;
    let left: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT principal_id FROM principal_grants")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(left, vec!["support".to_string()]);
}
