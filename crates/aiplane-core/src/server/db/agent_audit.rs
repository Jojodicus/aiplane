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
//!
//! #83 adds run events: every tool call made inside an agent run, with the
//! serialized [`RunChain`] and the decision that let it run or refused it.
//! Those are best-effort writes beside the call, not inside a transaction —
//! the call is authoritative, the row records it.

use jiff::Timestamp;
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use super::{DbError, Pool};
use crate::server::run_chain::RunChain;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditKind {
    PrincipalCreated,
    PrincipalDisabled,
    GrantAdded,
    GrantRemoved,
    TokenIssued,
    TokenRevoked,
    /// A tool call inside an agent run, allowed or denied.
    ToolCall,
    InjectionDetected,
    /// `forward_request`: every route's gate, and the route picked if any.
    RouteDecision,
    /// A sub-agent run started from a route.
    SubAgentDispatched,
    /// A sub-agent run ended, with its outcome.
    SubAgentFinished,
    AgentCreated,
    AgentDraftUpdated,
    AgentPublished,
    AgentLiveVersionSet,
    AgentShareSet,
    AgentShareRemoved,
    AgentDeleted,
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
            Self::ToolCall => "tool_call",
            Self::InjectionDetected => "injection_detected",
            Self::RouteDecision => "route_decision",
            Self::SubAgentDispatched => "sub_agent_dispatched",
            Self::SubAgentFinished => "sub_agent_finished",
            Self::AgentCreated => "agent_created",
            Self::AgentDraftUpdated => "agent_draft_updated",
            Self::AgentPublished => "agent_published",
            Self::AgentLiveVersionSet => "agent_live_version_set",
            Self::AgentShareSet => "agent_share_set",
            Self::AgentShareRemoved => "agent_share_removed",
            Self::AgentDeleted => "agent_deleted",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AuditEvent {
    pub id: String,
    pub kind: String,
    pub principal_id: String,
    pub actor_id: Option<String>,
    /// The run's call chain on a run event; `None` on a management event.
    pub chain: Option<Value>,
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
    insert(conn, kind, principal_id, Some(actor_id), None, detail).await
}

/// Write one row for something the principal itself ran into, with no acting
/// user: a run event, not a management change, so there is no transaction to
/// join. Inside an agent run, `chain` is its call chain and `principal_id`
/// its running frame's principal.
pub async fn record_run_event(
    pool: &Pool,
    kind: AuditKind,
    principal_id: &str,
    chain: Option<&RunChain>,
    detail: Value,
) -> Result<(), DbError> {
    let mut conn = pool.acquire().await?;
    insert(&mut conn, kind, principal_id, None, chain, detail).await
}

async fn insert(
    conn: &mut sqlx::SqliteConnection,
    kind: AuditKind,
    principal_id: &str,
    actor_id: Option<&str>,
    chain: Option<&RunChain>,
    detail: Value,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO agent_audit (id, kind, principal_id, actor_id, chain, detail, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(kind.as_str())
    .bind(principal_id)
    .bind(actor_id)
    .bind(chain.map(|c| c.to_json().to_string()))
    .bind(detail.to_string())
    .bind(Timestamp::now().to_string())
    .execute(conn)
    .await?;
    Ok(())
}

/// Newest first.
pub async fn for_principal(pool: &Pool, principal_id: &str) -> Result<Vec<AuditEvent>, DbError> {
    let rows = sqlx::query(
        "SELECT id, kind, principal_id, actor_id, chain, detail, created_at FROM agent_audit
          WHERE principal_id = ?
          ORDER BY created_at DESC, rowid DESC",
    )
    .bind(principal_id)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            let detail: String = row.try_get("detail")?;
            let chain: Option<String> = row.try_get("chain")?;
            Ok(AuditEvent {
                id: row.try_get("id")?,
                kind: row.try_get("kind")?,
                principal_id: row.try_get("principal_id")?,
                actor_id: row.try_get("actor_id")?,
                chain: chain.map(|c| serde_json::from_str(&c).unwrap_or(Value::String(c))),
                detail: serde_json::from_str(&detail).unwrap_or(Value::String(detail)),
                created_at: super::parse_ts(row.try_get("created_at")?, "created_at")?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::server::principal::{GrantSet, SystemPrincipal};
    use crate::server::run_chain::{CallSite, Frame};

    fn principal(id: &str, name: &str) -> SystemPrincipal {
        SystemPrincipal {
            id: id.into(),
            name: name.into(),
            grants: Arc::new(GrantSet::default()),
        }
    }

    #[tokio::test]
    async fn a_run_event_is_attributed_to_the_running_sub_agent_with_the_whole_chain() {
        let pool = crate::server::db::open(std::path::Path::new(":memory:"))
            .await
            .unwrap();
        let chain = RunChain::root(
            "s-visitor",
            Some("v-7".into()),
            Frame::for_principal(&principal("p-main", "support-website"), Some(2)),
        )
        .enter(
            Frame::for_principal(&principal("p-bill", "billing"), Some(5)).called_from(CallSite {
                turn_id: "t-main".into(),
                tool_call_id: "call-1".into(),
            }),
        )
        .unwrap();

        record_run_event(
            &pool,
            AuditKind::ToolCall,
            &chain.current().principal_id,
            Some(&chain),
            json!({"tool": "lookup_invoice", "decision": "allowed", "policy": "grant"}),
        )
        .await
        .unwrap();

        assert!(for_principal(&pool, "p-main").await.unwrap().is_empty());
        let events = for_principal(&pool, "p-bill").await.unwrap();
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.kind, "tool_call");
        assert_eq!(event.actor_id, None);
        assert_eq!(event.chain, Some(chain.to_json()));
        assert_eq!(event.detail["decision"], "allowed");
    }

    #[tokio::test]
    async fn a_management_event_has_no_chain() {
        let pool = crate::server::db::open(std::path::Path::new(":memory:"))
            .await
            .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        record(
            &mut conn,
            AuditKind::GrantAdded,
            "p1",
            "alice",
            json!({"kind": "tool"}),
        )
        .await
        .unwrap();
        drop(conn);
        let events = for_principal(&pool, "p1").await.unwrap();
        assert_eq!(events[0].chain, None);
        assert_eq!(events[0].actor_id.as_deref(), Some("alice"));
    }
}
