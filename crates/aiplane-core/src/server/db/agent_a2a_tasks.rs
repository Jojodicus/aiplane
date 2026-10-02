// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! A remote A2A task waiting for the visitor's input
//! (`migrations/0091_a2a_routes.sql`, `docs/agents.md` "What #101 built").
//!
//! Storage only. A route's call that paused on a remote `input-required`
//! records the remote task here; the resumed call takes it back out, once.

use sqlx::Row;

use super::{DbError, Pool};

/// The remote task a paused route call continues.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingTask {
    pub route: String,
    pub card_url: String,
    pub task_id: String,
    pub context_id: Option<String>,
}

/// Record that call `tool_call_id` of turn `turn_id` waits on `task`,
/// replacing an earlier wait of the same call.
pub async fn put(
    pool: &Pool,
    turn_id: &str,
    tool_call_id: &str,
    task: &PendingTask,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO agent_a2a_tasks (turn_id, tool_call_id, route, card_url, task_id, context_id, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(turn_id, tool_call_id) DO UPDATE SET
           route      = excluded.route,
           card_url   = excluded.card_url,
           task_id    = excluded.task_id,
           context_id = excluded.context_id,
           created_at = excluded.created_at",
    )
    .bind(turn_id)
    .bind(tool_call_id)
    .bind(&task.route)
    .bind(&task.card_url)
    .bind(&task.task_id)
    .bind(&task.context_id)
    .bind(jiff::Timestamp::now().to_string())
    .execute(pool)
    .await?;
    Ok(())
}

/// The task call `tool_call_id` of turn `turn_id` waits on, removed so it is
/// continued at most once.
pub async fn take(
    pool: &Pool,
    turn_id: &str,
    tool_call_id: &str,
) -> Result<Option<PendingTask>, DbError> {
    let row = sqlx::query(
        "DELETE FROM agent_a2a_tasks WHERE turn_id = ? AND tool_call_id = ?
         RETURNING route, card_url, task_id, context_id",
    )
    .bind(turn_id)
    .bind(tool_call_id)
    .fetch_optional(pool)
    .await?;
    row.map(|row| {
        Ok(PendingTask {
            route: row.try_get("route")?,
            card_url: row.try_get("card_url")?,
            task_id: row.try_get("task_id")?,
            context_id: row.try_get("context_id")?,
        })
    })
    .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::db;

    async fn turn(pool: &Pool) -> String {
        let now = jiff::Timestamp::now();
        db::users::upsert(
            pool,
            &db::users::User {
                id: "u1".into(),
                email: "u1@example.com".into(),
                name: None,
                roles: vec![],
                created_at: now,
                updated_at: now,
                timezone: None,
                speech_voice: None,
            },
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO chat_sessions (id, user_id, title, created_at, updated_at)
             VALUES ('s1', 'u1', 't', ?1, ?1)",
        )
        .bind(now.to_string())
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO chat_turns (id, session_id, seq, role, status, created_at)
             VALUES ('t1', 's1', 1, 'assistant', 'suspended', ?)",
        )
        .bind(now.to_string())
        .execute(pool)
        .await
        .unwrap();
        "t1".into()
    }

    fn task(id: &str) -> PendingTask {
        PendingTask {
            route: "partner".into(),
            card_url: "https://partner.example.com/.well-known/agent-card.json".into(),
            task_id: id.into(),
            context_id: Some("ctx-1".into()),
        }
    }

    #[tokio::test]
    async fn a_pending_task_is_taken_back_once() {
        let pool = db::open(std::path::Path::new(":memory:")).await.unwrap();
        let turn = turn(&pool).await;
        put(&pool, &turn, "call-1", &task("task-1")).await.unwrap();
        put(&pool, &turn, "call-1", &task("task-2")).await.unwrap();
        assert_eq!(take(&pool, &turn, "call-2").await.unwrap(), None);
        assert_eq!(
            take(&pool, &turn, "call-1").await.unwrap(),
            Some(task("task-2"))
        );
        assert_eq!(take(&pool, &turn, "call-1").await.unwrap(), None);
    }

    #[tokio::test]
    async fn deleting_the_turn_drops_its_pending_task() {
        let pool = db::open(std::path::Path::new(":memory:")).await.unwrap();
        let turn = turn(&pool).await;
        put(&pool, &turn, "call-1", &task("task-1")).await.unwrap();
        sqlx::query("DELETE FROM chat_turns WHERE id = ?")
            .bind(&turn)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(take(&pool, &turn, "call-1").await.unwrap(), None);
    }
}
