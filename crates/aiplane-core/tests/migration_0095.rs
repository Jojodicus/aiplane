// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Migration 0095 indexes `visitor_sessions.session_id`, the column the rate
//! windows join on and a chat session's deletion cascades by.

use aiplane_core::server::db;

#[tokio::test]
async fn a_visitor_session_is_found_by_its_chat_session_through_the_index() {
    let pool = db::open(std::path::Path::new(":memory:")).await.unwrap();
    let plan: Vec<(i64, i64, i64, String)> =
        sqlx::query_as("EXPLAIN QUERY PLAN SELECT id FROM visitor_sessions WHERE session_id = ?")
            .bind("s1")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(
        plan.iter()
            .any(|(_, _, _, d)| d.contains("visitor_sessions_session")),
        "{plan:?}"
    );
}
