// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! `agents`, their immutable `agent_versions` and their `agent_shares`
//! (`docs/agents.md` §2).
//!
//! An agent is keyed by its system principal: [`create`] writes both in one
//! transaction and [`delete`] removes the principal, which cascades to
//! everything here. The spec is stored as JSON text and never interpreted at
//! this layer — validating it is `aiplane-runtime`'s job, so the rows stay
//! below every consumer. Every mutation records an [`agent_audit`] row in the
//! same transaction.

use std::collections::HashMap;

use jiff::Timestamp;
use serde_json::json;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::agent_audit::{self, AuditKind};
use super::system_principals::{self as sp, NewPrincipal, PrincipalRow};
use super::{DbError, Pool};

/// What a share lets its holder do. `Write` includes `Read`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Access {
    Read,
    Write,
}

impl Access {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "read" => Some(Self::Read),
            "write" => Some(Self::Write),
            _ => None,
        }
    }
}

/// Who a share is for: one person (`users.id`) or a gateway group (its name).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubjectKind {
    User,
    Group,
}

impl SubjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Group => "group",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "user" => Some(Self::User),
            "group" => Some(Self::Group),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRow {
    pub principal: PrincipalRow,
    pub draft_spec: String,
    pub live_version: Option<i64>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionRow {
    pub version: i64,
    pub spec: String,
    pub published_by: String,
    pub published_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareRow {
    pub subject_kind: SubjectKind,
    pub subject_id: String,
    pub access: Access,
}

/// Why a share change was refused. Removing or downgrading the last `write`
/// share would leave an agent nobody can edit or share again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareChange {
    Changed,
    Unchanged,
    NotFound,
    LastWriter,
}

fn agent_cols() -> String {
    let principal = sp::PRINCIPAL_COLS
        .split(", ")
        .map(|c| format!("p.{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{principal}, a.draft_spec, a.live_version, a.created_at AS agent_created_at, \
         a.updated_at AS agent_updated_at"
    )
}

fn map_agent(row: &SqliteRow) -> Result<AgentRow, DbError> {
    Ok(AgentRow {
        principal: sp::map_principal(row)?,
        draft_spec: row.try_get("draft_spec")?,
        live_version: row.try_get("live_version")?,
        created_at: super::parse_ts(row.try_get("agent_created_at")?, "created_at")?,
        updated_at: super::parse_ts(row.try_get("agent_updated_at")?, "updated_at")?,
    })
}

fn map_version(row: &SqliteRow) -> Result<VersionRow, DbError> {
    Ok(VersionRow {
        version: row.try_get("version")?,
        spec: row.try_get("spec")?,
        published_by: row.try_get("published_by")?,
        published_at: super::parse_ts(row.try_get("published_at")?, "published_at")?,
    })
}

fn decode_share(row: &SqliteRow) -> Result<ShareRow, DbError> {
    let kind: String = row.try_get("subject_kind")?;
    let access: String = row.try_get("access")?;
    Ok(ShareRow {
        subject_kind: SubjectKind::parse(&kind).ok_or_else(|| DbError::Decode {
            column: "subject_kind",
            source: anyhow::anyhow!("unknown share subject kind `{kind}`"),
        })?,
        subject_id: row.try_get("subject_id")?,
        access: Access::parse(&access).ok_or_else(|| DbError::Decode {
            column: "access",
            source: anyhow::anyhow!("unknown share access `{access}`"),
        })?,
    })
}

/// Create the agent's principal (with no grants), the agent and the
/// creator's `write` share in one transaction. `Ok(None)` when the name is
/// taken.
pub async fn create(
    pool: &Pool,
    new: &NewPrincipal<'_>,
    draft_spec: &str,
    actor_id: &str,
) -> Result<Option<AgentRow>, DbError> {
    let now = Timestamp::now().to_string();
    let mut tx = pool.begin().await?;
    let Some(id) = sp::insert(&mut tx, new, actor_id).await? else {
        return Ok(None);
    };
    sqlx::query(
        "INSERT INTO agents (principal_id, draft_spec, live_version, created_at, updated_at)
         VALUES (?, ?, NULL, ?, ?)",
    )
    .bind(&id)
    .bind(draft_spec)
    .bind(&now)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO agent_shares (principal_id, subject_kind, subject_id, access)
         VALUES (?, 'user', ?, 'write')",
    )
    .bind(&id)
    .bind(actor_id)
    .execute(&mut *tx)
    .await?;
    agent_audit::record(
        &mut tx,
        AuditKind::AgentCreated,
        &id,
        actor_id,
        json!({ "name": new.name }),
    )
    .await?;
    tx.commit().await?;
    get(pool, &id).await
}

pub async fn get(pool: &Pool, id: &str) -> Result<Option<AgentRow>, DbError> {
    let row = sqlx::query(&format!(
        "SELECT {} FROM agents a JOIN system_principals p ON p.id = a.principal_id
          WHERE a.principal_id = ?",
        agent_cols()
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(map_agent).transpose()
}

/// Every agent id, mapped to whether it has a live version. What the spec
/// validator checks sub-agent references against.
pub async fn publication_status(pool: &Pool) -> Result<HashMap<String, bool>, DbError> {
    let rows = sqlx::query("SELECT principal_id, live_version IS NOT NULL AS live FROM agents")
        .fetch_all(pool)
        .await?;
    rows.iter()
        .map(|row| Ok((row.try_get("principal_id")?, row.try_get("live")?)))
        .collect()
}

/// `(sql, binds)` matching the shares held by a person directly or through
/// one of their groups.
fn holder_clause(user_id: &str, group_ids: &[String]) -> (String, Vec<String>) {
    let mut binds = vec![user_id.to_string()];
    let mut sql = "(s.subject_kind = 'user' AND s.subject_id = ?)".to_string();
    if !group_ids.is_empty() {
        let marks = vec!["?"; group_ids.len()].join(", ");
        sql = format!("({sql} OR (s.subject_kind = 'group' AND s.subject_id IN ({marks})))");
        binds.extend(group_ids.iter().cloned());
    }
    (sql, binds)
}

/// The strongest share a person holds on an agent, directly or through any of
/// `group_ids`. Whether it takes effect (the holder must have
/// `can_manage_agents`) is the caller's check.
pub async fn access_for(
    pool: &Pool,
    id: &str,
    user_id: &str,
    group_ids: &[String],
) -> Result<Option<Access>, DbError> {
    let (holder, binds) = holder_clause(user_id, group_ids);
    let sql = format!("SELECT s.access FROM agent_shares s WHERE s.principal_id = ? AND {holder}");
    let mut query = sqlx::query(&sql).bind(id);
    for b in &binds {
        query = query.bind(b);
    }
    let rows = query.fetch_all(pool).await?;
    let mut best = None;
    for row in &rows {
        let access: String = row.try_get("access")?;
        best = best.max(Access::parse(&access));
    }
    Ok(best)
}

/// The agents a person holds any share on, by name, with their strongest
/// access.
pub async fn list_shared_with(
    pool: &Pool,
    user_id: &str,
    group_ids: &[String],
) -> Result<Vec<(AgentRow, Access)>, DbError> {
    let (holder, binds) = holder_clause(user_id, group_ids);
    let sql = format!(
        "SELECT {}, MAX(CASE s.access WHEN 'write' THEN 1 ELSE 0 END) AS can_write
           FROM agents a
           JOIN system_principals p ON p.id = a.principal_id
           JOIN agent_shares s ON s.principal_id = a.principal_id
          WHERE {holder}
          GROUP BY a.principal_id
          ORDER BY p.name",
        agent_cols()
    );
    let mut query = sqlx::query(&sql);
    for b in &binds {
        query = query.bind(b);
    }
    let rows = query.fetch_all(pool).await?;
    rows.iter()
        .map(|row| {
            let access = if row.try_get::<i64, _>("can_write")? != 0 {
                Access::Write
            } else {
                Access::Read
            };
            Ok((map_agent(row)?, access))
        })
        .collect()
}

/// Every agent, by name — what an admin sees, shares or not.
pub async fn list_all(pool: &Pool) -> Result<Vec<AgentRow>, DbError> {
    let sql = format!(
        "SELECT {} FROM agents a JOIN system_principals p ON p.id = a.principal_id ORDER BY p.name",
        agent_cols()
    );
    let rows = sqlx::query(&sql).fetch_all(pool).await?;
    rows.iter().map(map_agent).collect()
}

/// Replace the draft. The live version is untouched. `Ok(false)` when there
/// is no such agent.
pub async fn update_draft(
    pool: &Pool,
    id: &str,
    draft_spec: &str,
    actor_id: &str,
) -> Result<bool, DbError> {
    let mut tx = pool.begin().await?;
    let changed =
        sqlx::query("UPDATE agents SET draft_spec = ?, updated_at = ? WHERE principal_id = ?")
            .bind(draft_spec)
            .bind(Timestamp::now().to_string())
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    if changed == 0 {
        return Ok(false);
    }
    agent_audit::record(
        &mut tx,
        AuditKind::AgentDraftUpdated,
        id,
        actor_id,
        json!({ "bytes": draft_spec.len() }),
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Snapshot `spec` — the draft as the caller validated it — as the next
/// version and make it live. Taking the text rather than re-reading the
/// draft means the version is exactly what passed validation, even if
/// someone saved the draft in between. `Ok(None)` when there is no such
/// agent.
pub async fn publish(
    pool: &Pool,
    id: &str,
    spec: &str,
    actor_id: &str,
) -> Result<Option<i64>, DbError> {
    let now = Timestamp::now().to_string();
    let mut tx = pool.begin().await?;
    let exists = sqlx::query("SELECT 1 FROM agents WHERE principal_id = ?")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .is_some();
    if !exists {
        return Ok(None);
    }
    let version: i64 = sqlx::query(
        "SELECT COALESCE(MAX(version), 0) + 1 AS next FROM agent_versions WHERE principal_id = ?",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?
    .try_get("next")?;
    sqlx::query(
        "INSERT INTO agent_versions (principal_id, version, spec, published_by, published_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(version)
    .bind(spec)
    .bind(actor_id)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE agents SET live_version = ?, updated_at = ? WHERE principal_id = ?")
        .bind(version)
        .bind(&now)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    agent_audit::record(
        &mut tx,
        AuditKind::AgentPublished,
        id,
        actor_id,
        json!({ "version": version }),
    )
    .await?;
    tx.commit().await?;
    Ok(Some(version))
}

/// Newest first.
pub async fn versions(pool: &Pool, id: &str) -> Result<Vec<VersionRow>, DbError> {
    let rows = sqlx::query(
        "SELECT version, spec, published_by, published_at FROM agent_versions
          WHERE principal_id = ? ORDER BY version DESC",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;
    rows.iter().map(map_version).collect()
}

pub async fn version(pool: &Pool, id: &str, version: i64) -> Result<Option<VersionRow>, DbError> {
    let row = sqlx::query(
        "SELECT version, spec, published_by, published_at FROM agent_versions
          WHERE principal_id = ? AND version = ?",
    )
    .bind(id)
    .bind(version)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(map_version).transpose()
}

/// The version number an internal test chat records for a run of the draft.
/// Published versions start at 1, so a session whose `agent_version` is 0 is a
/// test conversation, and no published version can ever match it.
pub const DRAFT_VERSION: i64 = 0;

/// The live version of agent `id` and its spec; `None` when it was never
/// published or does not exist.
pub async fn live(pool: &Pool, id: &str) -> Result<Option<(i64, String)>, DbError> {
    let row = sqlx::query(
        "SELECT v.version, v.spec FROM agents a
           JOIN agent_versions v ON v.principal_id = a.principal_id AND v.version = a.live_version
          WHERE a.principal_id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    row.map(|r| Ok((r.try_get("version")?, r.try_get("spec")?)))
        .transpose()
}

/// Every published agent's live spec, by agent id. What the spec validator
/// walks the sub-agent graph over.
pub async fn live_specs(pool: &Pool) -> Result<HashMap<String, String>, DbError> {
    let rows = sqlx::query(
        "SELECT a.principal_id, v.spec FROM agents a
           JOIN agent_versions v ON v.principal_id = a.principal_id AND v.version = a.live_version",
    )
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| Ok((row.try_get("principal_id")?, row.try_get("spec")?)))
        .collect()
}

/// Point `live_version` at an existing version — a rollback, or a roll
/// forward again. `Ok(false)` when the agent has no such version.
pub async fn set_live(
    pool: &Pool,
    id: &str,
    version: i64,
    actor_id: &str,
) -> Result<bool, DbError> {
    let mut tx = pool.begin().await?;
    let changed = sqlx::query(
        "UPDATE agents SET live_version = ?1, updated_at = ?2
          WHERE principal_id = ?3
            AND EXISTS (SELECT 1 FROM agent_versions WHERE principal_id = ?3 AND version = ?1)",
    )
    .bind(version)
    .bind(Timestamp::now().to_string())
    .bind(id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if changed == 0 {
        return Ok(false);
    }
    agent_audit::record(
        &mut tx,
        AuditKind::AgentLiveVersionSet,
        id,
        actor_id,
        json!({ "version": version }),
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

pub async fn shares(pool: &Pool, id: &str) -> Result<Vec<ShareRow>, DbError> {
    let rows = sqlx::query(
        "SELECT subject_kind, subject_id, access FROM agent_shares
          WHERE principal_id = ? ORDER BY subject_kind, subject_id",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;
    rows.iter().map(decode_share).collect()
}

async fn other_writers(
    conn: &mut sqlx::SqliteConnection,
    id: &str,
    kind: SubjectKind,
    subject_id: &str,
) -> Result<i64, DbError> {
    Ok(sqlx::query(
        "SELECT COUNT(*) AS n FROM agent_shares
          WHERE principal_id = ? AND access = 'write'
            AND NOT (subject_kind = ? AND subject_id = ?)",
    )
    .bind(id)
    .bind(kind.as_str())
    .bind(subject_id)
    .fetch_one(conn)
    .await?
    .try_get("n")?)
}

async fn current_share(
    conn: &mut sqlx::SqliteConnection,
    id: &str,
    kind: SubjectKind,
    subject_id: &str,
) -> Result<Option<Access>, DbError> {
    let row = sqlx::query(
        "SELECT access FROM agent_shares
          WHERE principal_id = ? AND subject_kind = ? AND subject_id = ?",
    )
    .bind(id)
    .bind(kind.as_str())
    .bind(subject_id)
    .fetch_optional(conn)
    .await?;
    Ok(row
        .map(|r| r.try_get::<String, _>("access"))
        .transpose()?
        .and_then(|a| Access::parse(&a)))
}

/// Add a share or change its access. Whether the holder may hold one at all
/// is the caller's check.
pub async fn set_share(
    pool: &Pool,
    id: &str,
    kind: SubjectKind,
    subject_id: &str,
    access: Access,
    actor_id: &str,
) -> Result<ShareChange, DbError> {
    let mut tx = pool.begin().await?;
    let current = current_share(&mut tx, id, kind, subject_id).await?;
    if current == Some(access) {
        return Ok(ShareChange::Unchanged);
    }
    if current == Some(Access::Write) && other_writers(&mut tx, id, kind, subject_id).await? == 0 {
        return Ok(ShareChange::LastWriter);
    }
    sqlx::query(
        "INSERT INTO agent_shares (principal_id, subject_kind, subject_id, access)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(principal_id, subject_kind, subject_id) DO UPDATE SET access = excluded.access",
    )
    .bind(id)
    .bind(kind.as_str())
    .bind(subject_id)
    .bind(access.as_str())
    .execute(&mut *tx)
    .await?;
    agent_audit::record(
        &mut tx,
        AuditKind::AgentShareSet,
        id,
        actor_id,
        json!({ "subject_kind": kind.as_str(), "subject_id": subject_id, "access": access.as_str() }),
    )
    .await?;
    tx.commit().await?;
    Ok(ShareChange::Changed)
}

pub async fn remove_share(
    pool: &Pool,
    id: &str,
    kind: SubjectKind,
    subject_id: &str,
    actor_id: &str,
) -> Result<ShareChange, DbError> {
    let mut tx = pool.begin().await?;
    let Some(current) = current_share(&mut tx, id, kind, subject_id).await? else {
        return Ok(ShareChange::NotFound);
    };
    if current == Access::Write && other_writers(&mut tx, id, kind, subject_id).await? == 0 {
        return Ok(ShareChange::LastWriter);
    }
    sqlx::query(
        "DELETE FROM agent_shares WHERE principal_id = ? AND subject_kind = ? AND subject_id = ?",
    )
    .bind(id)
    .bind(kind.as_str())
    .bind(subject_id)
    .execute(&mut *tx)
    .await?;
    agent_audit::record(
        &mut tx,
        AuditKind::AgentShareRemoved,
        id,
        actor_id,
        json!({ "subject_kind": kind.as_str(), "subject_id": subject_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(ShareChange::Changed)
}

/// Delete the agent together with its principal, grants, tokens, versions and
/// shares. The audit trail survives: it has no foreign keys. `Ok(false)` when
/// there is no such agent.
pub async fn delete(pool: &Pool, id: &str, actor_id: &str) -> Result<bool, DbError> {
    let mut tx = pool.begin().await?;
    let name: Option<String> = sqlx::query(
        "SELECT p.name FROM agents a JOIN system_principals p ON p.id = a.principal_id
          WHERE a.principal_id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .map(|r| r.try_get("name"))
    .transpose()?;
    let Some(name) = name else {
        return Ok(false);
    };
    sqlx::query("DELETE FROM system_principals WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    agent_audit::record(
        &mut tx,
        AuditKind::AgentDeleted,
        id,
        actor_id,
        json!({ "name": name }),
    )
    .await?;
    tx.commit().await?;
    super::embed_keys::origins_changed();
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::principal::GrantKind;
    use std::path::Path;

    async fn pool() -> Pool {
        super::super::open(Path::new(":memory:")).await.unwrap()
    }

    async fn agent(pool: &Pool, name: &str, creator: &str) -> AgentRow {
        create(
            pool,
            &NewPrincipal {
                name,
                display: "Support",
                description: "",
            },
            r#"{"main":{}}"#,
            creator,
        )
        .await
        .unwrap()
        .unwrap()
    }

    fn audit_kinds(events: &[agent_audit::AuditEvent]) -> Vec<&str> {
        events.iter().rev().map(|e| e.kind.as_str()).collect()
    }

    #[test]
    fn access_and_subject_kinds_round_trip_through_their_column_values() {
        for a in [Access::Read, Access::Write] {
            assert_eq!(Access::parse(a.as_str()), Some(a));
        }
        for k in [SubjectKind::User, SubjectKind::Group] {
            assert_eq!(SubjectKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(Access::parse("admin"), None);
        assert_eq!(SubjectKind::parse("role"), None);
        assert!(Access::Write > Access::Read);
    }

    #[tokio::test]
    async fn creating_an_agent_creates_its_principal_and_a_write_share_for_the_creator() {
        let pool = pool().await;
        let a = agent(&pool, "support", "alice").await;
        assert_eq!(a.principal.name, "support");
        assert_eq!(a.live_version, None);
        assert_eq!(a.draft_spec, r#"{"main":{}}"#);
        assert!(
            sp::get(&pool, &a.principal.id).await.unwrap().is_some(),
            "the principal exists"
        );
        assert!(sp::grants(&pool, &a.principal.id).await.unwrap().is_empty());
        assert_eq!(
            shares(&pool, &a.principal.id).await.unwrap(),
            [ShareRow {
                subject_kind: SubjectKind::User,
                subject_id: "alice".into(),
                access: Access::Write,
            }]
        );
        let audit = agent_audit::for_principal(&pool, &a.principal.id)
            .await
            .unwrap();
        assert_eq!(audit_kinds(&audit), ["principal_created", "agent_created"]);
    }

    #[tokio::test]
    async fn a_taken_name_creates_nothing() {
        let pool = pool().await;
        agent(&pool, "support", "alice").await;
        let second = create(
            &pool,
            &NewPrincipal {
                name: "support",
                display: "x",
                description: "",
            },
            "{}",
            "bob",
        )
        .await
        .unwrap();
        assert!(second.is_none());
        assert_eq!(publication_status(&pool).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn editing_the_draft_never_changes_the_live_version() {
        let pool = pool().await;
        let a = agent(&pool, "support", "alice").await;
        let id = &a.principal.id;
        assert_eq!(
            publish(&pool, id, "{\"v\":1}", "alice").await.unwrap(),
            Some(1)
        );
        assert!(update_draft(&pool, id, "{\"v\":2}", "alice").await.unwrap());

        let now = get(&pool, id).await.unwrap().unwrap();
        assert_eq!(now.draft_spec, "{\"v\":2}");
        assert_eq!(now.live_version, Some(1));
        assert_eq!(
            version(&pool, id, 1).await.unwrap().unwrap().spec,
            "{\"v\":1}"
        );
    }

    #[tokio::test]
    async fn the_live_spec_follows_the_live_pointer_and_lists_only_published_agents() {
        let pool = pool().await;
        let a = agent(&pool, "support", "alice").await;
        let draft_only = agent(&pool, "billing", "alice").await;
        let id = &a.principal.id;
        assert_eq!(live(&pool, id).await.unwrap(), None);
        publish(&pool, id, "one", "alice").await.unwrap();
        publish(&pool, id, "two", "alice").await.unwrap();
        assert_eq!(live(&pool, id).await.unwrap(), Some((2, "two".to_string())));
        set_live(&pool, id, 1, "alice").await.unwrap();
        assert_eq!(live(&pool, id).await.unwrap(), Some((1, "one".to_string())));

        let specs = live_specs(&pool).await.unwrap();
        assert_eq!(specs.get(id.as_str()).map(String::as_str), Some("one"));
        assert!(!specs.contains_key(&draft_only.principal.id));
        assert_eq!(live(&pool, "nope").await.unwrap(), None);
    }

    #[tokio::test]
    async fn publishing_numbers_versions_and_rollback_moves_only_the_live_pointer() {
        let pool = pool().await;
        let a = agent(&pool, "support", "alice").await;
        let id = &a.principal.id;
        assert_eq!(publish(&pool, id, "one", "alice").await.unwrap(), Some(1));
        assert_eq!(publish(&pool, id, "two", "bob").await.unwrap(), Some(2));
        assert_eq!(get(&pool, id).await.unwrap().unwrap().live_version, Some(2));

        assert!(set_live(&pool, id, 1, "alice").await.unwrap());
        assert_eq!(get(&pool, id).await.unwrap().unwrap().live_version, Some(1));
        assert!(!set_live(&pool, id, 7, "alice").await.unwrap());
        assert_eq!(get(&pool, id).await.unwrap().unwrap().live_version, Some(1));

        let all = versions(&pool, id).await.unwrap();
        assert_eq!(
            all.iter()
                .map(|v| (v.version, v.spec.as_str(), v.published_by.as_str()))
                .collect::<Vec<_>>(),
            [(2, "two", "bob"), (1, "one", "alice")]
        );
        assert_eq!(publish(&pool, "nope", "x", "alice").await.unwrap(), None);
        assert!(publication_status(&pool).await.unwrap()[id.as_str()]);

        let audit = agent_audit::for_principal(&pool, id).await.unwrap();
        assert_eq!(
            audit_kinds(&audit),
            [
                "principal_created",
                "agent_created",
                "agent_published",
                "agent_published",
                "agent_live_version_set"
            ]
        );
        assert_eq!(audit[0].detail["version"], 1);
    }

    #[tokio::test]
    async fn access_is_the_strongest_share_held_directly_or_through_a_group() {
        let pool = pool().await;
        let a = agent(&pool, "support", "alice").await;
        let id = &a.principal.id;
        let groups = |g: &[&str]| g.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            access_for(&pool, id, "alice", &[]).await.unwrap(),
            Some(Access::Write)
        );
        assert_eq!(access_for(&pool, id, "bob", &[]).await.unwrap(), None);

        set_share(
            &pool,
            id,
            SubjectKind::Group,
            "support-team",
            Access::Read,
            "alice",
        )
        .await
        .unwrap();
        set_share(&pool, id, SubjectKind::User, "bob", Access::Write, "alice")
            .await
            .unwrap();
        assert_eq!(
            access_for(&pool, id, "carol", &groups(&["support-team"]))
                .await
                .unwrap(),
            Some(Access::Read)
        );
        assert_eq!(
            access_for(&pool, id, "bob", &groups(&["support-team"]))
                .await
                .unwrap(),
            Some(Access::Write)
        );

        let listed = list_shared_with(&pool, "carol", &groups(&["support-team", "x"]))
            .await
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].1, Access::Read);
        assert!(
            list_shared_with(&pool, "dave", &[])
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(list_all(&pool).await.unwrap().len(), 1);
        let bob = list_shared_with(&pool, "bob", &groups(&["support-team"]))
            .await
            .unwrap();
        assert_eq!(bob[0].1, Access::Write);
    }

    #[tokio::test]
    async fn the_last_write_share_cannot_be_removed_or_downgraded() {
        let pool = pool().await;
        let a = agent(&pool, "support", "alice").await;
        let id = &a.principal.id;
        assert_eq!(
            remove_share(&pool, id, SubjectKind::User, "alice", "alice")
                .await
                .unwrap(),
            ShareChange::LastWriter
        );
        assert_eq!(
            set_share(&pool, id, SubjectKind::User, "alice", Access::Read, "alice")
                .await
                .unwrap(),
            ShareChange::LastWriter
        );
        assert_eq!(
            set_share(&pool, id, SubjectKind::User, "bob", Access::Write, "alice")
                .await
                .unwrap(),
            ShareChange::Changed
        );
        assert_eq!(
            set_share(&pool, id, SubjectKind::User, "bob", Access::Write, "alice")
                .await
                .unwrap(),
            ShareChange::Unchanged
        );
        assert_eq!(
            remove_share(&pool, id, SubjectKind::User, "alice", "bob")
                .await
                .unwrap(),
            ShareChange::Changed
        );
        assert_eq!(
            remove_share(&pool, id, SubjectKind::User, "alice", "bob")
                .await
                .unwrap(),
            ShareChange::NotFound
        );
        let audit = agent_audit::for_principal(&pool, id).await.unwrap();
        assert_eq!(audit[0].kind, "agent_share_removed");
        assert_eq!(audit[0].actor_id.as_deref(), Some("bob"));
        assert_eq!(audit[1].kind, "agent_share_set");
        assert_eq!(audit[1].detail["access"], "write");
    }

    #[tokio::test]
    async fn deleting_an_agent_removes_its_principal_but_keeps_the_audit_trail() {
        let pool = pool().await;
        let a = agent(&pool, "support", "alice").await;
        let id = &a.principal.id;
        sp::add_grant(&pool, id, GrantKind::Pool, "chat", "alice")
            .await
            .unwrap();
        publish(&pool, id, "{}", "alice").await.unwrap();

        assert!(delete(&pool, id, "alice").await.unwrap());
        assert!(!delete(&pool, id, "alice").await.unwrap());
        assert!(get(&pool, id).await.unwrap().is_none());
        assert!(sp::get(&pool, id).await.unwrap().is_none());
        assert!(sp::grants(&pool, id).await.unwrap().is_empty());
        assert!(versions(&pool, id).await.unwrap().is_empty());
        assert!(shares(&pool, id).await.unwrap().is_empty());
        let audit = agent_audit::for_principal(&pool, id).await.unwrap();
        assert_eq!(audit[0].kind, "agent_deleted");
        assert_eq!(audit[0].detail["name"], "support");
    }

    #[tokio::test]
    async fn a_plain_system_principal_is_not_an_agent() {
        let pool = pool().await;
        let p = sp::create(
            &pool,
            &NewPrincipal {
                name: "ci",
                display: "CI",
                description: "",
            },
            "admin",
        )
        .await
        .unwrap()
        .unwrap();
        assert!(get(&pool, &p.id).await.unwrap().is_none());
        assert!(!delete(&pool, &p.id, "admin").await.unwrap());
        assert!(sp::get(&pool, &p.id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn the_schema_refuses_unknown_share_kinds_and_access() {
        let pool = pool().await;
        let a = agent(&pool, "support", "alice").await;
        for (kind, access) in [("role", "read"), ("user", "admin")] {
            let res = sqlx::query(
                "INSERT INTO agent_shares (principal_id, subject_kind, subject_id, access)
                 VALUES (?, ?, 'x', ?)",
            )
            .bind(&a.principal.id)
            .bind(kind)
            .bind(access)
            .execute(&pool)
            .await;
            assert!(res.is_err(), "{kind}/{access} accepted");
        }
    }
}
