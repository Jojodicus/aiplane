// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Stored responses: the `api_responses` table.
//!
//! A response is stored when its request had `store` on (OpenAI's default),
//! so that `previous_response_id` can continue it and its owner can read or
//! delete it. Every read is scoped to the owner — the person or system
//! principal whose token created it — so another caller's id answers exactly
//! like an id that never existed. Rows live [`RETENTION_SECS`], OpenAI's own
//! retention for stored responses; expired rows are never served, and
//! [`spawn_pruner`] deletes them.

use std::time::Duration;

use serde_json::Value;

use aiplane_core::server::db::Pool;
use aiplane_core::server::principal::Principal;

/// Thirty days.
pub const RETENTION_SECS: i64 = 30 * 24 * 60 * 60;

/// How many stored responses one request may continue, and how many bytes of
/// them. Bounds, so a chain is always read in finite work and memory; the byte
/// bound is the largest body one request may send, so a chain may carry as
/// much context as a client could have sent inline.
pub const MAX_CHAIN: usize = 1000;
pub const MAX_CHAIN_BYTES: usize = crate::rama_server::body_limit::UPLOAD_MAX_BODY_BYTES;

/// How often expired rows are deleted.
const PRUNE_EVERY: Duration = Duration::from_secs(60 * 60);

/// Whose stored responses a request may see.
#[derive(Debug, Clone)]
pub struct Owner {
    user_id: Option<String>,
    principal_id: Option<String>,
}

impl Owner {
    pub fn of(principal: &Principal) -> Self {
        Self {
            user_id: principal.user_id().map(str::to_owned),
            principal_id: principal.system().map(|sp| sp.id.clone()),
        }
    }
}

/// One stored response.
#[derive(Debug, Clone)]
pub struct Stored {
    pub response: Value,
    pub input_items: Vec<Value>,
    pub previous_response_id: Option<String>,
    /// The stored JSON's size, which a chain's byte bound counts.
    bytes: usize,
}

/// Why a chain could not be read.
#[derive(Debug)]
pub enum ChainError {
    NotFound(String),
    TooLong(String),
    Db(sqlx::Error),
}

impl std::fmt::Display for ChainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(id) => write!(f, "Previous response with id '{id}' not found."),
            Self::TooLong(id) => write!(
                f,
                "the conversation chain ending at '{id}' is longer than {MAX_CHAIN} responses \
                 or {} MiB; start a new chain by sending the conversation as `input`",
                MAX_CHAIN_BYTES / (1024 * 1024)
            ),
            Self::Db(err) => write!(f, "reading stored responses: {err}"),
        }
    }
}

impl From<sqlx::Error> for ChainError {
    fn from(err: sqlx::Error) -> Self {
        Self::Db(err)
    }
}

/// Store a response.
pub async fn save(
    pool: &Pool,
    owner: &Owner,
    previous_response_id: Option<&str>,
    input_items: &[Value],
    response: &Value,
) -> Result<(), sqlx::Error> {
    let now = super::output::now_unix();
    sqlx::query(
        "INSERT INTO api_responses
             (id, user_id, principal_id, previous_response_id,
              input_items, response, created_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(response["id"].as_str().unwrap_or_default())
    .bind(&owner.user_id)
    .bind(&owner.principal_id)
    .bind(previous_response_id)
    .bind(Value::Array(input_items.to_vec()).to_string())
    .bind(response.to_string())
    .bind(now)
    .bind(now + RETENTION_SECS)
    .execute(pool)
    .await?;
    Ok(())
}

/// Delete every row past its expiry. Returns how many went.
pub async fn prune(pool: &Pool) -> Result<u64, sqlx::Error> {
    let done = sqlx::query("DELETE FROM api_responses WHERE expires_at <= ?")
        .bind(super::output::now_unix())
        .execute(pool)
        .await?;
    Ok(done.rows_affected())
}

/// Prune expired rows now and every [`PRUNE_EVERY`], so the retention holds
/// whether or not anything new is stored.
pub fn spawn_pruner(pool: Pool) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(PRUNE_EVERY);
        loop {
            tick.tick().await;
            if let Err(err) = prune(&pool).await {
                tracing::warn!(error = %err, "pruning expired stored responses failed");
            }
        }
    });
}

/// The owner's unexpired response `id`, if there is one.
pub async fn find(pool: &Pool, owner: &Owner, id: &str) -> Result<Option<Stored>, sqlx::Error> {
    let row: Option<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT response, input_items, previous_response_id FROM api_responses
          WHERE id = ? AND expires_at > ?
            AND (user_id IS ? AND principal_id IS ?)",
    )
    .bind(id)
    .bind(super::output::now_unix())
    .bind(&owner.user_id)
    .bind(&owner.principal_id)
    .fetch_optional(pool)
    .await?;
    Ok(
        row.map(|(response, input_items, previous_response_id)| Stored {
            bytes: response.len() + input_items.len(),
            response: serde_json::from_str(&response).unwrap_or(Value::Null),
            input_items: serde_json::from_str(&input_items).unwrap_or_default(),
            previous_response_id,
        }),
    )
}

/// Delete the owner's response `id`. `false` when there was none to delete.
pub async fn delete(pool: &Pool, owner: &Owner, id: &str) -> Result<bool, sqlx::Error> {
    let done = sqlx::query(
        "DELETE FROM api_responses
          WHERE id = ? AND expires_at > ?
            AND (user_id IS ? AND principal_id IS ?)",
    )
    .bind(id)
    .bind(super::output::now_unix())
    .bind(&owner.user_id)
    .bind(&owner.principal_id)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Every item of the chain ending at `id`, oldest first: each response's input
/// items followed by its output. A chain with a missing link is an error
/// rather than a shorter context, because the model would otherwise answer a
/// conversation the caller never had.
pub async fn history(pool: &Pool, owner: &Owner, id: &str) -> Result<Vec<Value>, ChainError> {
    let mut chain = Vec::new();
    let mut bytes = 0;
    let mut next = Some(id.to_string());
    while let Some(current) = next {
        if chain.len() == MAX_CHAIN {
            return Err(ChainError::TooLong(id.to_string()));
        }
        let stored = find(pool, owner, &current)
            .await?
            .ok_or(ChainError::NotFound(current))?;
        bytes += stored.bytes;
        if bytes > MAX_CHAIN_BYTES {
            return Err(ChainError::TooLong(id.to_string()));
        }
        next = stored.previous_response_id.clone();
        chain.push(stored);
    }
    Ok(chain
        .into_iter()
        .rev()
        .flat_map(|stored| {
            let output = stored.response["output"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            stored.input_items.into_iter().chain(output)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn pool() -> Pool {
        let pool = aiplane_core::server::db::open(std::path::Path::new(":memory:"))
            .await
            .unwrap();
        for user in ["alice", "bob"] {
            sqlx::query(
                "INSERT INTO users (id, email, created_at, updated_at) VALUES (?, ?, '', '')",
            )
            .bind(user)
            .bind(format!("{user}@x"))
            .execute(&pool)
            .await
            .unwrap();
        }
        pool
    }

    fn owner(user: &str) -> Owner {
        Owner {
            user_id: Some(user.to_string()),
            principal_id: None,
        }
    }

    fn response(id: &str, text: &str) -> Value {
        json!({"id": id, "object": "response", "output": [
            {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]},
        ]})
    }

    fn said(text: &str) -> Value {
        json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": text}]})
    }

    #[tokio::test]
    async fn a_stored_response_is_found_by_its_owner_only() {
        let pool = pool().await;
        save(
            &pool,
            &owner("alice"),
            None,
            &[said("hi")],
            &response("resp_1", "hello"),
        )
        .await
        .unwrap();
        let found = find(&pool, &owner("alice"), "resp_1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.response["output"][0]["content"][0]["text"], "hello");
        assert_eq!(found.input_items, vec![said("hi")]);
        assert!(
            find(&pool, &owner("bob"), "resp_1")
                .await
                .unwrap()
                .is_none()
        );
        assert!(!delete(&pool, &owner("bob"), "resp_1").await.unwrap());
        assert!(delete(&pool, &owner("alice"), "resp_1").await.unwrap());
        assert!(
            find(&pool, &owner("alice"), "resp_1")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_chain_is_read_oldest_first() {
        let pool = pool().await;
        save(
            &pool,
            &owner("alice"),
            None,
            &[said("one")],
            &response("resp_1", "uno"),
        )
        .await
        .unwrap();
        save(
            &pool,
            &owner("alice"),
            Some("resp_1"),
            &[said("two")],
            &response("resp_2", "dos"),
        )
        .await
        .unwrap();
        let items = history(&pool, &owner("alice"), "resp_2").await.unwrap();
        let texts: Vec<&str> = items
            .iter()
            .map(|i| i["content"][0]["text"].as_str().unwrap())
            .collect();
        assert_eq!(texts, ["one", "uno", "two", "dos"]);
    }

    #[tokio::test]
    async fn a_chain_with_a_missing_link_is_an_error() {
        let pool = pool().await;
        save(
            &pool,
            &owner("alice"),
            None,
            &[said("one")],
            &response("resp_1", "uno"),
        )
        .await
        .unwrap();
        save(
            &pool,
            &owner("alice"),
            Some("resp_1"),
            &[],
            &response("resp_2", "dos"),
        )
        .await
        .unwrap();
        delete(&pool, &owner("alice"), "resp_1").await.unwrap();
        assert!(matches!(
            history(&pool, &owner("alice"), "resp_2").await,
            Err(ChainError::NotFound(id)) if id == "resp_1"
        ));
        assert!(matches!(
            history(&pool, &owner("bob"), "resp_2").await,
            Err(ChainError::NotFound(id)) if id == "resp_2"
        ));
    }

    #[tokio::test]
    async fn expired_rows_are_not_served_and_are_pruned() {
        let pool = pool().await;
        save(
            &pool,
            &owner("alice"),
            None,
            &[],
            &response("resp_old", "x"),
        )
        .await
        .unwrap();
        sqlx::query("UPDATE api_responses SET expires_at = 0")
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            find(&pool, &owner("alice"), "resp_old")
                .await
                .unwrap()
                .is_none()
        );
        save(
            &pool,
            &owner("alice"),
            None,
            &[],
            &response("resp_new", "y"),
        )
        .await
        .unwrap();
        assert_eq!(prune(&pool).await.unwrap(), 1);
        let (rows,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM api_responses")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 1, "only the unexpired row is left");
    }

    #[tokio::test]
    async fn a_chain_over_the_byte_bound_is_too_long() {
        let pool = pool().await;
        let big = "x".repeat(MAX_CHAIN_BYTES / 2 + 1);
        save(
            &pool,
            &owner("alice"),
            None,
            &[said(&big)],
            &response("resp_1", "a"),
        )
        .await
        .unwrap();
        save(
            &pool,
            &owner("alice"),
            Some("resp_1"),
            &[said(&big)],
            &response("resp_2", "b"),
        )
        .await
        .unwrap();
        assert!(history(&pool, &owner("alice"), "resp_1").await.is_ok());
        assert!(matches!(
            history(&pool, &owner("alice"), "resp_2").await,
            Err(ChainError::TooLong(id)) if id == "resp_2"
        ));
    }

    #[tokio::test]
    async fn deleting_the_owner_deletes_their_responses() {
        let pool = pool().await;
        save(&pool, &owner("alice"), None, &[], &response("resp_1", "x"))
            .await
            .unwrap();
        sqlx::query("DELETE FROM users WHERE id = 'alice'")
            .execute(&pool)
            .await
            .unwrap();
        let (rows,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM api_responses")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
    }
}
