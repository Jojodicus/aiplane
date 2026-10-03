// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! `agent_responders`: who may answer an agent's approvals and handoffs in the
//! inbox without a share (`docs/agents.md` "What #96 built").
//!
//! A share shows the spec and every conversation, so it needs the
//! agent-management permission. Support staff who only answer what the agent
//! asks for need neither: a responder sees the pending item and its minimal
//! context, nothing else. Adding and removing one records an [`agent_audit`]
//! row in the same transaction.

use jiff::Timestamp;
use serde_json::json;
use sqlx::Row;

use super::agent_audit::{self, AuditKind};
use super::agents::SubjectKind;
use super::{DbError, Pool, WriteTx};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Responder {
    pub subject_kind: SubjectKind,
    pub subject_id: String,
    pub added_by: String,
    pub added_at: Timestamp,
}

pub async fn list(pool: &Pool, agent_id: &str) -> Result<Vec<Responder>, DbError> {
    let rows = sqlx::query(
        "SELECT subject_kind, subject_id, added_by, added_at FROM agent_responders
          WHERE principal_id = ? ORDER BY subject_kind, subject_id",
    )
    .bind(agent_id)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            let kind: String = row.try_get("subject_kind")?;
            Ok(Responder {
                subject_kind: SubjectKind::parse(&kind).ok_or_else(|| DbError::Decode {
                    column: "subject_kind",
                    source: anyhow::anyhow!("unknown responder kind `{kind}`"),
                })?,
                subject_id: row.try_get("subject_id")?,
                added_by: row.try_get("added_by")?,
                added_at: super::parse_ts(row.try_get("added_at")?, "added_at")?,
            })
        })
        .collect()
}

/// Add a responder. `Ok(false)` when they already are one.
pub async fn add(
    pool: &Pool,
    agent_id: &str,
    kind: SubjectKind,
    subject_id: &str,
    actor_id: &str,
) -> Result<bool, DbError> {
    let mut tx = WriteTx::begin(pool).await?;
    let added = sqlx::query(
        "INSERT INTO agent_responders (principal_id, subject_kind, subject_id, added_by, added_at)
         VALUES (?, ?, ?, ?, ?) ON CONFLICT DO NOTHING",
    )
    .bind(agent_id)
    .bind(kind.as_str())
    .bind(subject_id)
    .bind(actor_id)
    .bind(Timestamp::now().to_string())
    .execute(&mut *tx)
    .await?
    .rows_affected()
        > 0;
    if added {
        agent_audit::append(
            &mut tx,
            agent_audit::NewEvent::new(
                AuditKind::ResponderAdded,
                agent_id,
                json!({ "subject_kind": kind.as_str(), "subject_id": subject_id }),
            )
            .by(Some(actor_id)),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(added)
}

/// Remove a responder. `Ok(false)` when they were none.
pub async fn remove(
    pool: &Pool,
    agent_id: &str,
    kind: SubjectKind,
    subject_id: &str,
    actor_id: &str,
) -> Result<bool, DbError> {
    let mut tx = WriteTx::begin(pool).await?;
    let removed = sqlx::query(
        "DELETE FROM agent_responders
          WHERE principal_id = ? AND subject_kind = ? AND subject_id = ?",
    )
    .bind(agent_id)
    .bind(kind.as_str())
    .bind(subject_id)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        > 0;
    if removed {
        agent_audit::append(
            &mut tx,
            agent_audit::NewEvent::new(
                AuditKind::ResponderRemoved,
                agent_id,
                json!({ "subject_kind": kind.as_str(), "subject_id": subject_id }),
            )
            .by(Some(actor_id)),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(removed)
}

/// Whether a person is a responder of the agent, directly or through any of
/// `group_ids`.
pub async fn is_responder(
    pool: &Pool,
    agent_id: &str,
    user_id: &str,
    group_ids: &[String],
) -> Result<bool, DbError> {
    Ok(list(pool, agent_id)
        .await?
        .iter()
        .any(|r| match r.subject_kind {
            SubjectKind::User => r.subject_id == user_id,
            SubjectKind::Group => group_ids.contains(&r.subject_id),
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{agents, system_principals as sp};
    use std::path::Path;

    async fn pool_with_agent() -> (Pool, String) {
        let pool = aiplane_core::server::db::open(Path::new(":memory:"))
            .await
            .unwrap();
        let id = agents::create(
            &pool,
            &sp::NewPrincipal {
                name: "support",
                display: "Support",
                description: "",
            },
            "{}",
            "alice",
        )
        .await
        .unwrap()
        .unwrap()
        .principal
        .id;
        (pool, id)
    }

    #[tokio::test]
    async fn a_responder_is_matched_directly_or_through_a_group() {
        let (pool, agent) = pool_with_agent().await;
        assert!(
            add(&pool, &agent, SubjectKind::User, "sam", "alice")
                .await
                .unwrap()
        );
        assert!(
            add(&pool, &agent, SubjectKind::Group, "support", "alice")
                .await
                .unwrap()
        );
        assert!(
            !add(&pool, &agent, SubjectKind::User, "sam", "alice")
                .await
                .unwrap()
        );

        assert!(is_responder(&pool, &agent, "sam", &[]).await.unwrap());
        assert!(
            is_responder(&pool, &agent, "kim", &["support".into()])
                .await
                .unwrap()
        );
        assert!(
            !is_responder(&pool, &agent, "kim", &["sales".into()])
                .await
                .unwrap()
        );
        assert_eq!(list(&pool, &agent).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn adding_and_removing_is_audited_once() {
        let (pool, agent) = pool_with_agent().await;
        add(&pool, &agent, SubjectKind::User, "sam", "alice")
            .await
            .unwrap();
        add(&pool, &agent, SubjectKind::User, "sam", "alice")
            .await
            .unwrap();
        assert!(
            remove(&pool, &agent, SubjectKind::User, "sam", "alice")
                .await
                .unwrap()
        );
        assert!(
            !remove(&pool, &agent, SubjectKind::User, "sam", "alice")
                .await
                .unwrap()
        );
        let kinds: Vec<String> = agent_audit::for_principal(&pool, &agent)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .filter(|k| k.starts_with("responder"))
            .collect();
        assert_eq!(kinds.len(), 2, "{kinds:?}");
        assert!(!is_responder(&pool, &agent, "sam", &[]).await.unwrap());
    }
}
