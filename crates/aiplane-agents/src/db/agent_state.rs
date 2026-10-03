// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Rows of `agent_state`: the typed slots of one agent conversation
//! (`migrations/0082_agent_state.sql`, `docs/agents.md` §3 "State").
//!
//! Storage only. Nothing here knows a slot's type or who may write it; that is
//! `aiplane-runtime::agents::state`, which is the one caller. Writing through
//! [`put`] directly would skip both the validator and the `set_by` check, so
//! nothing else may.

use aiplane_core::server::run_chain::RunChain;
use jiff::Timestamp;
use serde_json::{Value, json};
use sqlx::Row;

use super::agent_audit::{self, AuditKind, Correlation, NewEvent};
use super::{DbError, Pool};

#[derive(Debug, Clone, PartialEq)]
pub struct StoredSlot {
    pub slot: String,
    pub value: Value,
    /// `llm`, `verifier:<id>` or `host`.
    pub provenance: String,
    pub set_at: Timestamp,
}

/// Insert or replace one slot of `session_id` on `conn` — inside a
/// transaction, which this takes the write lock of — and record the write
/// in the activity log on the same transaction: the slot, its old and new
/// value, the provenance, and when. The write and its event commit together
/// or not at all. A slot of a person's conversation is written without an
/// event; only an agent's conversation has a log. `run` is the run that
/// wrote it: its root conversation's chain takes the event, so a sub-agent's
/// write in its child session lands where the rest of the run is.
pub async fn put(
    conn: &mut sqlx::SqliteConnection,
    session_id: &str,
    slot: &str,
    value: &Value,
    provenance: &str,
    set_at: Timestamp,
    run: Option<&RunChain>,
) -> Result<(), DbError> {
    // A write first, so the old value is read under the lock it is replaced
    // under.
    sqlx::query("UPDATE agent_state SET value = value WHERE session_id = ? AND slot = ?")
        .bind(session_id)
        .bind(slot)
        .execute(&mut *conn)
        .await?;
    let old: Option<(String, String, String)> = sqlx::query_as(
        "SELECT value, provenance, set_at FROM agent_state WHERE session_id = ? AND slot = ?",
    )
    .bind(session_id)
    .bind(slot)
    .fetch_optional(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT INTO agent_state (session_id, slot, value, provenance, set_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(session_id, slot) DO UPDATE SET
           value      = excluded.value,
           provenance = excluded.provenance,
           set_at     = excluded.set_at",
    )
    .bind(session_id)
    .bind(slot)
    .bind(value.to_string())
    .bind(provenance)
    .bind(set_at.to_string())
    .execute(&mut *conn)
    .await?;
    let owner: Option<String> =
        sqlx::query_scalar("SELECT principal_id FROM chat_sessions WHERE id = ?")
            .bind(session_id)
            .fetch_optional(&mut *conn)
            .await?
            .flatten();
    let Some(owner) = owner else {
        return Ok(());
    };
    let old = old.map(|(value, provenance, set_at)| {
        json!({
            "value": serde_json::from_str::<Value>(&value).unwrap_or(Value::String(value)),
            "provenance": provenance,
            "set_at": set_at,
        })
    });
    agent_audit::append(
        conn,
        NewEvent::new(
            AuditKind::StateWritten,
            &owner,
            json!({
                "slot": slot,
                "old": old,
                "new": value,
                "provenance": provenance,
                "writer": provenance,
                "set_at": set_at,
            }),
        )
        .in_run(run)
        .at(Correlation {
            session_id: Some(session_id.to_string()),
            conversation_id: Some(session_id.to_string()),
            ..Correlation::default()
        }),
    )
    .await?;
    Ok(())
}

/// Every written slot of `session_id`, by slot name.
pub async fn for_session(pool: &Pool, session_id: &str) -> Result<Vec<StoredSlot>, DbError> {
    let rows = sqlx::query(
        "SELECT slot, value, provenance, set_at FROM agent_state
          WHERE session_id = ?
          ORDER BY slot",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            let value: String = row.try_get("value")?;
            Ok(StoredSlot {
                slot: row.try_get("slot")?,
                value: serde_json::from_str(&value).map_err(|e| DbError::Decode {
                    column: "value",
                    source: e.into(),
                })?,
                provenance: row.try_get("provenance")?,
                set_at: super::parse_ts(row.try_get("set_at")?, "set_at")?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiplane_core::server::db::open;
    use serde_json::json;
    use std::path::Path;

    async fn fresh() -> Pool {
        open(Path::new(":memory:")).await.unwrap()
    }

    async fn seed_session(pool: &Pool, id: &str) {
        sqlx::query(
            "INSERT INTO users (id, email, created_at, updated_at)
             VALUES ('u1', 'u1@example.com', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')
             ON CONFLICT(id) DO NOTHING",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO chat_sessions (id, user_id, created_at, updated_at)
             VALUES (?, 'u1', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[tokio::test]
    async fn a_written_slot_reads_back_with_its_provenance_and_time() {
        let pool = fresh().await;
        seed_session(&pool, "s1").await;
        put(
            &mut pool.acquire().await.unwrap(),
            "s1",
            "verified",
            &json!({"customer_id": "K-12345"}),
            "verifier:otp",
            at("2026-10-02T10:00:00Z"),
            None,
        )
        .await
        .unwrap();
        put(
            &mut pool.acquire().await.unwrap(),
            "s1",
            "email",
            &json!("a@b.example"),
            "llm",
            at("2026-10-02T10:01:00Z"),
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            for_session(&pool, "s1").await.unwrap(),
            [
                StoredSlot {
                    slot: "email".into(),
                    value: json!("a@b.example"),
                    provenance: "llm".into(),
                    set_at: at("2026-10-02T10:01:00Z"),
                },
                StoredSlot {
                    slot: "verified".into(),
                    value: json!({"customer_id": "K-12345"}),
                    provenance: "verifier:otp".into(),
                    set_at: at("2026-10-02T10:00:00Z"),
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_rewrite_replaces_value_provenance_and_time() {
        let pool = fresh().await;
        seed_session(&pool, "s1").await;
        put(
            &mut pool.acquire().await.unwrap(),
            "s1",
            "issue",
            &json!("sales"),
            "llm",
            at("2026-10-02T10:00:00Z"),
            None,
        )
        .await
        .unwrap();
        put(
            &mut pool.acquire().await.unwrap(),
            "s1",
            "issue",
            &json!("billing"),
            "host",
            at("2026-10-02T11:00:00Z"),
            None,
        )
        .await
        .unwrap();
        let rows = for_session(&pool, "s1").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].value, json!("billing"));
        assert_eq!(rows[0].provenance, "host");
        assert_eq!(rows[0].set_at, at("2026-10-02T11:00:00Z"));
    }

    #[tokio::test]
    async fn state_is_scoped_to_its_session_and_dies_with_it() {
        let pool = fresh().await;
        seed_session(&pool, "s1").await;
        seed_session(&pool, "s2").await;
        put(
            &mut pool.acquire().await.unwrap(),
            "s1",
            "name",
            &json!("Ada"),
            "llm",
            Timestamp::now(),
            None,
        )
        .await
        .unwrap();
        assert!(for_session(&pool, "s2").await.unwrap().is_empty());

        sqlx::query("DELETE FROM chat_sessions WHERE id = 's1'")
            .execute(&pool)
            .await
            .unwrap();
        assert!(for_session(&pool, "s1").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_table_refuses_a_provenance_that_is_not_one_of_the_three() {
        let pool = fresh().await;
        seed_session(&pool, "s1").await;
        for bad in ["model", "verifier:", "LLM", ""] {
            assert!(
                put(
                    &mut pool.acquire().await.unwrap(),
                    "s1",
                    "x",
                    &json!(1),
                    bad,
                    Timestamp::now(),
                    None
                )
                .await
                .is_err(),
                "{bad:?} was accepted"
            );
        }
    }

    #[tokio::test]
    async fn a_row_for_a_session_that_does_not_exist_is_refused() {
        let pool = fresh().await;
        assert!(
            put(
                &mut pool.acquire().await.unwrap(),
                "ghost",
                "x",
                &json!(1),
                "llm",
                Timestamp::now(),
                None
            )
            .await
            .is_err()
        );
    }
}
