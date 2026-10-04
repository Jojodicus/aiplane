// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! `agent_architect_sessions`: which of a person's chats run as the agent
//! architect, and which agent each one plans (`docs/agent-builder.md` → "Agent architect"). The conversation itself is an ordinary chat of the person's.

use jiff::Timestamp;
use sqlx::Row;

use super::{DbError, Pool};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchitectSession {
    pub session_id: String,
    pub user_id: String,
    /// The agent it plans; `None` until it has created one.
    pub agent_id: Option<String>,
}

/// Mark `session_id` (a chat of `user_id`'s) as an architect conversation.
pub async fn create(
    pool: &Pool,
    session_id: &str,
    user_id: &str,
    agent_id: Option<&str>,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO agent_architect_sessions (session_id, user_id, agent_id, created_at)
         VALUES (?, ?, ?, ?)",
    )
    .bind(session_id)
    .bind(user_id)
    .bind(agent_id)
    .bind(Timestamp::now().to_string())
    .execute(pool)
    .await?;
    Ok(())
}

/// The architect conversation `session_id`, `None` for any other chat.
pub async fn get(pool: &Pool, session_id: &str) -> Result<Option<ArchitectSession>, DbError> {
    let row = sqlx::query(
        "SELECT session_id, user_id, agent_id FROM agent_architect_sessions WHERE session_id = ?",
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await?;
    row.map(|r| {
        Ok(ArchitectSession {
            session_id: r.try_get("session_id")?,
            user_id: r.try_get("user_id")?,
            agent_id: r.try_get("agent_id")?,
        })
    })
    .transpose()
}

/// `user_id`'s newest architect conversation about `agent_id` (about no
/// agent yet, for `None`).
pub async fn latest(
    pool: &Pool,
    user_id: &str,
    agent_id: Option<&str>,
) -> Result<Option<String>, DbError> {
    Ok(sqlx::query_scalar(
        "SELECT session_id FROM agent_architect_sessions
          WHERE user_id = ? AND agent_id IS ?
          ORDER BY created_at DESC, rowid DESC LIMIT 1",
    )
    .bind(user_id)
    .bind(agent_id)
    .fetch_optional(pool)
    .await?)
}

/// The conversation now plans `agent_id` (the one it just created).
pub async fn set_agent(pool: &Pool, session_id: &str, agent_id: &str) -> Result<(), DbError> {
    sqlx::query("UPDATE agent_architect_sessions SET agent_id = ? WHERE session_id = ?")
        .bind(agent_id)
        .bind(session_id)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::agents;
    use crate::db::system_principals::NewPrincipal;
    use std::path::Path;

    async fn pool() -> Pool {
        aiplane_core::server::db::open(Path::new(":memory:"))
            .await
            .unwrap()
    }

    async fn person(pool: &Pool, id: &str) {
        let now = Timestamp::now();
        aiplane_core::server::db::users::upsert(
            pool,
            &aiplane_core::server::db::users::User {
                id: id.into(),
                email: format!("{id}@example.com"),
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
    }

    #[tokio::test]
    async fn an_architect_conversation_is_found_by_its_person_and_agent() {
        let pool = pool().await;
        person(&pool, "alice").await;
        let agent = agents::create(
            &pool,
            &NewPrincipal {
                name: "support",
                display: "Support",
                description: "",
            },
            "{}",
            "alice",
        )
        .await
        .unwrap()
        .unwrap();
        let agent_id = agent.principal.id.as_str();
        let unplanned = session_core::db::create_session(&pool, "alice")
            .await
            .unwrap();
        let planned = session_core::db::create_session(&pool, "alice")
            .await
            .unwrap();
        let ordinary = session_core::db::create_session(&pool, "alice")
            .await
            .unwrap();
        create(&pool, &unplanned.id, "alice", None).await.unwrap();
        create(&pool, &planned.id, "alice", Some(agent_id))
            .await
            .unwrap();

        assert_eq!(
            latest(&pool, "alice", Some(agent_id)).await.unwrap(),
            Some(planned.id.clone())
        );
        assert_eq!(
            latest(&pool, "alice", None).await.unwrap(),
            Some(unplanned.id.clone())
        );
        assert_eq!(latest(&pool, "bob", None).await.unwrap(), None);
        assert_eq!(get(&pool, &ordinary.id).await.unwrap(), None);

        set_agent(&pool, &unplanned.id, agent_id).await.unwrap();
        let moved = get(&pool, &unplanned.id).await.unwrap().unwrap();
        assert_eq!(moved.agent_id.as_deref(), Some(agent_id));
        assert_eq!(latest(&pool, "alice", None).await.unwrap(), None);

        agents::delete(&pool, agent_id, "alice").await.unwrap();
        assert_eq!(
            get(&pool, &planned.id).await.unwrap().unwrap().agent_id,
            None,
            "the conversation outlives the agent it planned"
        );
        session_core::db::delete_session(&pool, "alice", &planned.id)
            .await
            .unwrap();
        assert_eq!(get(&pool, &planned.id).await.unwrap(), None);
    }
}
