// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Migration 0094 indexes suspension deadlines so the 30-second expiry sweep
//! reads only the expired rows instead of the whole table.

use aiplane_core::server::db;

#[tokio::test]
async fn the_expiry_sweep_searches_the_deadline_index() {
    let pool = db::open(std::path::Path::new(":memory:")).await.unwrap();
    let plan: Vec<(i64, i64, i64, String)> = sqlx::query_as(
        "EXPLAIN QUERY PLAN SELECT turn_id FROM chat_turn_suspensions WHERE expires_at < ?",
    )
    .bind("2026-10-02T10:00:00Z")
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(
        plan.iter()
            .any(|(_, _, _, d)| d.contains("chat_turn_suspensions_expires_at")),
        "{plan:?}"
    );
}
