// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! `system_principals`, their `principal_grants` and their `system_tokens`.
//!
//! Every write that changes what a principal can do or authenticate with
//! records an [`agent_audit`] row in the same transaction. See
//! `docs/agents.md` §1 for why principals are a separate table from users.

use jiff::Timestamp;
use serde_json::json;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;
use uuid::Uuid;

use super::agent_audit::{self, AuditKind};
use super::{DbError, Pool};
use crate::server::principal::{GrantKind, GrantSet, SystemPrincipal};

const MAX_NAME_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrincipalRow {
    pub id: String,
    pub name: String,
    pub display: String,
    pub description: String,
    pub created_by: String,
    pub created_at: Timestamp,
    pub disabled_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantRow {
    pub kind: GrantKind,
    pub reference: String,
    pub granted_by: String,
    pub granted_at: Timestamp,
}

/// A token row without its hash — nothing outside the auth lookup needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemToken {
    pub id: String,
    pub principal_id: String,
    pub name: String,
    pub created_by: String,
    pub created_at: Timestamp,
    pub last_used_at: Option<Timestamp>,
    pub expires_at: Timestamp,
    pub revoked_at: Option<Timestamp>,
}

pub struct NewPrincipal<'a> {
    pub name: &'a str,
    pub display: &'a str,
    pub description: &'a str,
}

/// Why a principal name is unusable, phrased for the person typing it.
/// `None` when the name is fine. Names show up in usage and audit, so they
/// are slugs: lowercase ASCII letters, digits and `-`.
pub fn invalid_name_reason(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("a principal needs a name".into());
    }
    if name.len() > MAX_NAME_LEN {
        return Some(format!(
            "the principal name `{name}` is longer than {MAX_NAME_LEN} characters"
        ));
    }
    let slug = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !slug || name.starts_with('-') || name.ends_with('-') {
        return Some(format!(
            "the principal name `{name}` is not a slug — use lowercase letters, digits and \
             `-`, e.g. `support-website`"
        ));
    }
    None
}

pub(crate) fn map_principal(row: &SqliteRow) -> Result<PrincipalRow, DbError> {
    Ok(PrincipalRow {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        display: row.try_get("display")?,
        description: row.try_get("description")?,
        created_by: row.try_get("created_by")?,
        created_at: super::parse_ts(row.try_get("created_at")?, "created_at")?,
        disabled_at: super::parse_optional_ts(row.try_get("disabled_at")?, "disabled_at")?,
    })
}

fn map_token(row: &SqliteRow) -> Result<SystemToken, DbError> {
    Ok(SystemToken {
        id: row.try_get("id")?,
        principal_id: row.try_get("principal_id")?,
        name: row.try_get("name")?,
        created_by: row.try_get("created_by")?,
        created_at: super::parse_ts(row.try_get("created_at")?, "created_at")?,
        last_used_at: super::parse_optional_ts(row.try_get("last_used_at")?, "last_used_at")?,
        expires_at: super::parse_ts(row.try_get("expires_at")?, "expires_at")?,
        revoked_at: super::parse_optional_ts(row.try_get("revoked_at")?, "revoked_at")?,
    })
}

pub(crate) const PRINCIPAL_COLS: &str =
    "id, name, display, description, created_by, created_at, disabled_at";
const TOKEN_COLS: &str =
    "id, principal_id, name, created_by, created_at, last_used_at, expires_at, revoked_at";

/// Create a principal with no grants. `Ok(None)` when the name is taken.
pub async fn create(
    pool: &Pool,
    new: &NewPrincipal<'_>,
    actor_id: &str,
) -> Result<Option<PrincipalRow>, DbError> {
    let mut tx = pool.begin().await?;
    let Some(id) = insert(&mut tx, new, actor_id).await? else {
        return Ok(None);
    };
    tx.commit().await?;
    get(pool, &id).await
}

/// Write the principal row and its `principal_created` audit row on the
/// caller's transaction, so an agent can create its principal atomically with
/// itself. `Ok(None)` when the name is taken.
pub(crate) async fn insert(
    conn: &mut sqlx::SqliteConnection,
    new: &NewPrincipal<'_>,
    actor_id: &str,
) -> Result<Option<String>, DbError> {
    let id = Uuid::new_v4().to_string();
    let inserted = sqlx::query(
        "INSERT INTO system_principals (id, name, display, description, created_by, created_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(name) DO NOTHING",
    )
    .bind(&id)
    .bind(new.name)
    .bind(new.display)
    .bind(new.description)
    .bind(actor_id)
    .bind(Timestamp::now().to_string())
    .execute(&mut *conn)
    .await?
    .rows_affected();
    if inserted == 0 {
        return Ok(None);
    }
    agent_audit::record(
        conn,
        AuditKind::PrincipalCreated,
        &id,
        actor_id,
        json!({ "name": new.name }),
    )
    .await?;
    Ok(Some(id))
}

pub async fn list(pool: &Pool) -> Result<Vec<PrincipalRow>, DbError> {
    let rows = sqlx::query(&format!(
        "SELECT {PRINCIPAL_COLS} FROM system_principals ORDER BY name"
    ))
    .fetch_all(pool)
    .await?;
    rows.iter().map(map_principal).collect()
}

pub async fn get(pool: &Pool, id: &str) -> Result<Option<PrincipalRow>, DbError> {
    let row = sqlx::query(&format!(
        "SELECT {PRINCIPAL_COLS} FROM system_principals WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(map_principal).transpose()
}

/// Disable a principal and revoke every token it holds, in one transaction.
/// `Ok(false)` when there is no such principal or it was already disabled.
pub async fn disable(pool: &Pool, id: &str, actor_id: &str) -> Result<bool, DbError> {
    let now = Timestamp::now().to_string();
    let mut tx = pool.begin().await?;
    let changed = sqlx::query(
        "UPDATE system_principals SET disabled_at = ? WHERE id = ? AND disabled_at IS NULL",
    )
    .bind(&now)
    .bind(id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if changed == 0 {
        return Ok(false);
    }
    let revoked = sqlx::query(
        "UPDATE system_tokens SET revoked_at = ? WHERE principal_id = ? AND revoked_at IS NULL",
    )
    .bind(&now)
    .bind(id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    agent_audit::record(
        &mut tx,
        AuditKind::PrincipalDisabled,
        id,
        actor_id,
        json!({ "tokens_revoked": revoked }),
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

pub async fn grants(pool: &Pool, principal_id: &str) -> Result<Vec<GrantRow>, DbError> {
    let rows = sqlx::query(
        "SELECT kind, ref, granted_by, granted_at FROM principal_grants
          WHERE principal_id = ? ORDER BY kind, ref",
    )
    .bind(principal_id)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            let kind: String = row.try_get("kind")?;
            Ok(GrantRow {
                kind: GrantKind::parse(&kind).ok_or_else(|| DbError::Decode {
                    column: "kind",
                    source: anyhow::anyhow!("unknown grant kind `{kind}`"),
                })?,
                reference: row.try_get("ref")?,
                granted_by: row.try_get("granted_by")?,
                granted_at: super::parse_ts(row.try_get("granted_at")?, "granted_at")?,
            })
        })
        .collect()
}

/// Add one grant. `Ok(false)` when it was already held — nothing changes and
/// nothing is audited.
pub async fn add_grant(
    pool: &Pool,
    principal_id: &str,
    kind: GrantKind,
    reference: &str,
    actor_id: &str,
) -> Result<bool, DbError> {
    let mut tx = pool.begin().await?;
    let inserted = sqlx::query(
        "INSERT INTO principal_grants (principal_id, kind, ref, granted_by, granted_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(principal_id, kind, ref) DO NOTHING",
    )
    .bind(principal_id)
    .bind(kind.as_str())
    .bind(reference)
    .bind(actor_id)
    .bind(Timestamp::now().to_string())
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if inserted == 0 {
        return Ok(false);
    }
    agent_audit::record(
        &mut tx,
        AuditKind::GrantAdded,
        principal_id,
        actor_id,
        json!({ "kind": kind.as_str(), "ref": reference }),
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Remove one grant. `Ok(false)` when it was not held.
pub async fn remove_grant(
    pool: &Pool,
    principal_id: &str,
    kind: GrantKind,
    reference: &str,
    actor_id: &str,
) -> Result<bool, DbError> {
    let mut tx = pool.begin().await?;
    let removed =
        sqlx::query("DELETE FROM principal_grants WHERE principal_id = ? AND kind = ? AND ref = ?")
            .bind(principal_id)
            .bind(kind.as_str())
            .bind(reference)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    if removed == 0 {
        return Ok(false);
    }
    agent_audit::record(
        &mut tx,
        AuditKind::GrantRemoved,
        principal_id,
        actor_id,
        json!({ "kind": kind.as_str(), "ref": reference }),
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// The principal as a request runs it: identity plus its grants. `None` when
/// it does not exist or is disabled.
pub async fn load_active(pool: &Pool, id: &str) -> Result<Option<SystemPrincipal>, DbError> {
    let Some(row) = get(pool, id).await?.filter(|p| p.disabled_at.is_none()) else {
        return Ok(None);
    };
    let grants = grants(pool, id).await?;
    Ok(Some(SystemPrincipal {
        id: row.id,
        name: row.name,
        grants: std::sync::Arc::new(GrantSet::new(
            grants.into_iter().map(|g| (g.kind, g.reference)),
        )),
    }))
}

pub async fn insert_token(
    pool: &Pool,
    principal_id: &str,
    name: &str,
    hash: &str,
    expires_at: Timestamp,
    actor_id: &str,
) -> Result<SystemToken, DbError> {
    let id = Uuid::new_v4().to_string();
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO system_tokens (id, principal_id, name, hash, created_by, created_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(principal_id)
    .bind(name)
    .bind(hash)
    .bind(actor_id)
    .bind(Timestamp::now().to_string())
    .bind(expires_at.to_string())
    .execute(&mut *tx)
    .await?;
    agent_audit::record(
        &mut tx,
        AuditKind::TokenIssued,
        principal_id,
        actor_id,
        json!({ "token_id": id, "name": name }),
    )
    .await?;
    tx.commit().await?;
    let row = sqlx::query(&format!(
        "SELECT {TOKEN_COLS} FROM system_tokens WHERE id = ?"
    ))
    .bind(&id)
    .fetch_one(pool)
    .await?;
    map_token(&row)
}

pub async fn tokens(pool: &Pool, principal_id: &str) -> Result<Vec<SystemToken>, DbError> {
    let rows = sqlx::query(&format!(
        "SELECT {TOKEN_COLS} FROM system_tokens WHERE principal_id = ? ORDER BY created_at"
    ))
    .bind(principal_id)
    .fetch_all(pool)
    .await?;
    rows.iter().map(map_token).collect()
}

/// Revoke one of the principal's tokens. `Ok(false)` when no live token with
/// that id belongs to it.
pub async fn revoke_token(
    pool: &Pool,
    principal_id: &str,
    token_id: &str,
    actor_id: &str,
) -> Result<bool, DbError> {
    let mut tx = pool.begin().await?;
    let changed = sqlx::query(
        "UPDATE system_tokens SET revoked_at = ?
          WHERE id = ? AND principal_id = ? AND revoked_at IS NULL",
    )
    .bind(Timestamp::now().to_string())
    .bind(token_id)
    .bind(principal_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if changed == 0 {
        return Ok(false);
    }
    agent_audit::record(
        &mut tx,
        AuditKind::TokenRevoked,
        principal_id,
        actor_id,
        json!({ "token_id": token_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// The live token behind a bearer hash: not revoked, not expired, and owned
/// by a principal that is not disabled.
pub async fn find_active_token_by_hash(
    pool: &Pool,
    hash: &str,
) -> Result<Option<SystemToken>, DbError> {
    let row = sqlx::query(
        "SELECT t.id, t.principal_id, t.name, t.created_by, t.created_at, t.last_used_at,
                t.expires_at, t.revoked_at
           FROM system_tokens t
           JOIN system_principals p ON p.id = t.principal_id
          WHERE t.hash = ?
            AND t.revoked_at IS NULL
            AND t.expires_at > ?
            AND p.disabled_at IS NULL",
    )
    .bind(hash)
    .bind(Timestamp::now().to_string())
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(map_token).transpose()
}

pub async fn touch_token(pool: &Pool, token_id: &str) -> Result<(), DbError> {
    sqlx::query("UPDATE system_tokens SET last_used_at = ? WHERE id = ?")
        .bind(Timestamp::now().to_string())
        .bind(token_id)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::auth::token;
    use std::path::Path;

    async fn pool() -> Pool {
        super::super::open(Path::new(":memory:")).await.unwrap()
    }

    async fn principal(pool: &Pool, name: &str) -> PrincipalRow {
        create(
            pool,
            &NewPrincipal {
                name,
                display: "CI",
                description: "",
            },
            "admin",
        )
        .await
        .unwrap()
        .unwrap()
    }

    fn in_days(days: i64) -> Timestamp {
        Timestamp::now() + jiff::SignedDuration::from_hours(24 * days)
    }

    #[test]
    fn names_must_be_slugs() {
        assert_eq!(invalid_name_reason("support-website"), None);
        assert_eq!(invalid_name_reason("ci2"), None);
        for bad in ["", "Support", "a b", "-x", "x-", "ä", &"a".repeat(65)] {
            assert!(invalid_name_reason(bad).is_some(), "{bad:?} accepted");
        }
    }

    #[tokio::test]
    async fn a_new_principal_has_no_grants_and_its_creation_is_audited() {
        let pool = pool().await;
        let p = principal(&pool, "ci").await;
        assert!(grants(&pool, &p.id).await.unwrap().is_empty());
        let loaded = load_active(&pool, &p.id).await.unwrap().unwrap();
        assert!(loaded.grants.is_empty());
        assert_eq!(loaded.name, "ci");
        let audit = agent_audit::for_principal(&pool, &p.id).await.unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].kind, "principal_created");
        assert_eq!(audit[0].actor_id.as_deref(), Some("admin"));
    }

    #[tokio::test]
    async fn a_taken_name_is_reported_not_overwritten() {
        let pool = pool().await;
        let first = principal(&pool, "ci").await;
        let second = create(
            &pool,
            &NewPrincipal {
                name: "ci",
                display: "Other",
                description: "",
            },
            "admin",
        )
        .await
        .unwrap();
        assert!(second.is_none());
        assert_eq!(get(&pool, &first.id).await.unwrap().unwrap().display, "CI");
    }

    #[tokio::test]
    async fn grants_are_added_and_removed_with_an_audit_row_each() {
        let pool = pool().await;
        let p = principal(&pool, "ci").await;
        assert!(
            add_grant(&pool, &p.id, GrantKind::Tool, "time", "alice")
                .await
                .unwrap()
        );
        assert!(
            !add_grant(&pool, &p.id, GrantKind::Tool, "time", "bob")
                .await
                .unwrap()
        );
        assert!(
            add_grant(&pool, &p.id, GrantKind::Pool, "chat", "alice")
                .await
                .unwrap()
        );

        let held = grants(&pool, &p.id).await.unwrap();
        assert_eq!(held.len(), 2);
        let time = held.iter().find(|g| g.kind == GrantKind::Tool).unwrap();
        assert_eq!(time.reference, "time");
        assert_eq!(time.granted_by, "alice");

        assert!(
            remove_grant(&pool, &p.id, GrantKind::Tool, "time", "bob")
                .await
                .unwrap()
        );
        assert!(
            !remove_grant(&pool, &p.id, GrantKind::Tool, "time", "bob")
                .await
                .unwrap()
        );
        let loaded = load_active(&pool, &p.id).await.unwrap().unwrap();
        assert!(!loaded.grants.has(GrantKind::Tool, "time"));
        assert!(loaded.grants.has(GrantKind::Pool, "chat"));

        let kinds: Vec<(String, Option<String>)> = agent_audit::for_principal(&pool, &p.id)
            .await
            .unwrap()
            .into_iter()
            .rev()
            .map(|e| (e.kind, e.actor_id))
            .collect();
        assert_eq!(
            kinds,
            [
                ("principal_created".to_string(), Some("admin".to_string())),
                ("grant_added".to_string(), Some("alice".to_string())),
                ("grant_added".to_string(), Some("alice".to_string())),
                ("grant_removed".to_string(), Some("bob".to_string())),
            ]
        );
    }

    #[tokio::test]
    async fn a_token_authenticates_until_revoked() {
        let pool = pool().await;
        let p = principal(&pool, "ci").await;
        let (_, hash) = token::mint_system();
        let t = insert_token(&pool, &p.id, "pipeline", &hash, in_days(30), "admin")
            .await
            .unwrap();
        let found = find_active_token_by_hash(&pool, &hash)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.id, t.id);
        assert_eq!(found.principal_id, p.id);

        assert!(revoke_token(&pool, &p.id, &t.id, "admin").await.unwrap());
        assert!(
            find_active_token_by_hash(&pool, &hash)
                .await
                .unwrap()
                .is_none()
        );
        assert!(!revoke_token(&pool, &p.id, &t.id, "admin").await.unwrap());
    }

    #[tokio::test]
    async fn an_expired_token_does_not_authenticate() {
        let pool = pool().await;
        let p = principal(&pool, "ci").await;
        let (_, hash) = token::mint_system();
        insert_token(&pool, &p.id, "old", &hash, in_days(-1), "admin")
            .await
            .unwrap();
        assert!(
            find_active_token_by_hash(&pool, &hash)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn another_principal_cannot_revoke_a_token_it_does_not_own() {
        let pool = pool().await;
        let a = principal(&pool, "a").await;
        let b = principal(&pool, "b").await;
        let (_, hash) = token::mint_system();
        let t = insert_token(&pool, &a.id, "t", &hash, in_days(30), "admin")
            .await
            .unwrap();
        assert!(!revoke_token(&pool, &b.id, &t.id, "admin").await.unwrap());
        assert!(
            find_active_token_by_hash(&pool, &hash)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn disabling_revokes_every_token_and_hides_the_principal_from_runs() {
        let pool = pool().await;
        let p = principal(&pool, "ci").await;
        let (_, h1) = token::mint_system();
        let (_, h2) = token::mint_system();
        insert_token(&pool, &p.id, "one", &h1, in_days(30), "admin")
            .await
            .unwrap();
        insert_token(&pool, &p.id, "two", &h2, in_days(30), "admin")
            .await
            .unwrap();

        assert!(disable(&pool, &p.id, "admin").await.unwrap());
        assert!(!disable(&pool, &p.id, "admin").await.unwrap());
        assert!(
            find_active_token_by_hash(&pool, &h1)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            find_active_token_by_hash(&pool, &h2)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            tokens(&pool, &p.id)
                .await
                .unwrap()
                .iter()
                .all(|t| t.revoked_at.is_some())
        );
        assert!(load_active(&pool, &p.id).await.unwrap().is_none());
        assert!(
            get(&pool, &p.id)
                .await
                .unwrap()
                .unwrap()
                .disabled_at
                .is_some()
        );
        let audit = agent_audit::for_principal(&pool, &p.id).await.unwrap();
        assert_eq!(audit[0].kind, "principal_disabled");
        assert_eq!(audit[0].detail["tokens_revoked"], 2);
    }

    #[tokio::test]
    async fn the_unknown_grant_kind_is_rejected_by_the_schema() {
        let pool = pool().await;
        let p = principal(&pool, "ci").await;
        let err = sqlx::query(
            "INSERT INTO principal_grants (principal_id, kind, ref, granted_by, granted_at)
             VALUES (?, 'model', 'x', 'a', '2026-01-01T00:00:00Z')",
        )
        .bind(&p.id)
        .execute(&pool)
        .await;
        assert!(err.is_err());
    }
}
