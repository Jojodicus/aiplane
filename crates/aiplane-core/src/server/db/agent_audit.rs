// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Append-only trail of what was done to or by a system principal.
//!
//! #77 writes the management events — who created or disabled a principal,
//! who granted or revoked what, who issued or revoked a token. Rows are
//! written inside the same transaction as the change they describe, so a
//! grant can never exist without its audit row. No foreign keys
//! (`migrations/0077_system_principals.sql`): the trail outlives both the
//! principal and the acting user.

use jiff::Timestamp;
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use super::{DbError, Pool};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditKind {
    PrincipalCreated,
    PrincipalDisabled,
    GrantAdded,
    GrantRemoved,
    TokenIssued,
    TokenRevoked,
    InjectionDetected,
}

impl AuditKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PrincipalCreated => "principal_created",
            Self::PrincipalDisabled => "principal_disabled",
            Self::GrantAdded => "grant_added",
            Self::GrantRemoved => "grant_removed",
            Self::TokenIssued => "token_issued",
            Self::TokenRevoked => "token_revoked",
            Self::InjectionDetected => "injection_detected",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AuditEvent {
    pub id: String,
    pub kind: String,
    pub principal_id: String,
    pub actor_id: Option<String>,
    pub detail: Value,
    pub created_at: Timestamp,
}

/// Write one row on `conn` — the caller's transaction, so the row commits or
/// rolls back with the change it records.
pub async fn record(
    conn: &mut sqlx::SqliteConnection,
    kind: AuditKind,
    principal_id: &str,
    actor_id: &str,
    detail: Value,
) -> Result<(), DbError> {
    insert(conn, kind, principal_id, Some(actor_id), detail).await
}

/// Write one row for something the principal itself ran into, with no acting
/// user: a run event, not a management change, so there is no transaction to
/// join.
pub async fn record_run_event(
    pool: &Pool,
    kind: AuditKind,
    principal_id: &str,
    detail: Value,
) -> Result<(), DbError> {
    let mut conn = pool.acquire().await?;
    insert(&mut conn, kind, principal_id, None, detail).await
}

async fn insert(
    conn: &mut sqlx::SqliteConnection,
    kind: AuditKind,
    principal_id: &str,
    actor_id: Option<&str>,
    detail: Value,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO agent_audit (id, kind, principal_id, actor_id, chain, detail, created_at)
         VALUES (?, ?, ?, ?, NULL, ?, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(kind.as_str())
    .bind(principal_id)
    .bind(actor_id)
    .bind(detail.to_string())
    .bind(Timestamp::now().to_string())
    .execute(conn)
    .await?;
    Ok(())
}

/// Newest first.
pub async fn for_principal(pool: &Pool, principal_id: &str) -> Result<Vec<AuditEvent>, DbError> {
    let rows = sqlx::query(
        "SELECT id, kind, principal_id, actor_id, detail, created_at FROM agent_audit
          WHERE principal_id = ?
          ORDER BY created_at DESC, rowid DESC",
    )
    .bind(principal_id)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            let detail: String = row.try_get("detail")?;
            Ok(AuditEvent {
                id: row.try_get("id")?,
                kind: row.try_get("kind")?,
                principal_id: row.try_get("principal_id")?,
                actor_id: row.try_get("actor_id")?,
                detail: serde_json::from_str(&detail).unwrap_or(Value::String(detail)),
                created_at: super::parse_ts(row.try_get("created_at")?, "created_at")?,
            })
        })
        .collect()
}
