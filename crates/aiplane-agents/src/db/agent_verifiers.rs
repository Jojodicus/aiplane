// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Rows the identity verifiers keep (`migrations/0088_agent_verifiers.sql`,
//! `docs/agents.md` "What #95 built").
//!
//! Storage only: the outstanding code of a verifier in a conversation (never
//! the code itself, which the customer's connector owns), the send and lookup
//! events the rate windows count, and the host identity tokens already
//! accepted. What the numbers mean is `aiplane-runtime::agents::verifier`.

use jiff::{SignedDuration, Timestamp};
use sqlx::Row;
use uuid::Uuid;

use super::{DbError, Pool};

/// The longest rate window a spec may configure. Events older than this are
/// never counted again, so recording one prunes them.
pub const MAX_WINDOW: SignedDuration = SignedDuration::from_hours(24);

/// The one outstanding code of a verifier in a conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingCode {
    pub email_hash: String,
    pub sent_at: Timestamp,
    pub expires_at: Timestamp,
    /// Checks made against this code so far.
    pub attempts: u32,
}

/// Record that a fresh code went to `email_hash`, replacing any earlier one
/// and its attempt count.
pub async fn put_code(
    pool: &Pool,
    session_id: &str,
    verifier: &str,
    email_hash: &str,
    sent_at: Timestamp,
    expires_at: Timestamp,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO agent_verifier_codes (session_id, verifier, email_hash, sent_at, expires_at, attempts)
         VALUES (?, ?, ?, ?, ?, 0)
         ON CONFLICT(session_id, verifier) DO UPDATE SET
           email_hash = excluded.email_hash,
           sent_at    = excluded.sent_at,
           expires_at = excluded.expires_at,
           attempts   = 0",
    )
    .bind(session_id)
    .bind(verifier)
    .bind(email_hash)
    .bind(sent_at.to_string())
    .bind(expires_at.to_string())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn pending_code(
    pool: &Pool,
    session_id: &str,
    verifier: &str,
) -> Result<Option<PendingCode>, DbError> {
    let row = sqlx::query(
        "SELECT email_hash, sent_at, expires_at, attempts FROM agent_verifier_codes
          WHERE session_id = ? AND verifier = ?",
    )
    .bind(session_id)
    .bind(verifier)
    .fetch_optional(pool)
    .await?;
    row.map(|row| {
        let attempts: i64 = row.try_get("attempts")?;
        Ok(PendingCode {
            email_hash: row.try_get("email_hash")?,
            sent_at: super::parse_ts(row.try_get("sent_at")?, "sent_at")?,
            expires_at: super::parse_ts(row.try_get("expires_at")?, "expires_at")?,
            attempts: u32::try_from(attempts).unwrap_or(u32::MAX),
        })
    })
    .transpose()
}

/// Count one more check of the outstanding code, unless it already had
/// `max` — in one statement, so two checks racing cannot both take the last
/// attempt. `Some(n)` is the attempt this check is; `None` means no code is
/// outstanding or it is used up.
pub async fn take_attempt(
    pool: &Pool,
    session_id: &str,
    verifier: &str,
    max: u32,
) -> Result<Option<u32>, DbError> {
    let row = sqlx::query(
        "UPDATE agent_verifier_codes SET attempts = attempts + 1
          WHERE session_id = ? AND verifier = ? AND attempts < ?
          RETURNING attempts",
    )
    .bind(session_id)
    .bind(verifier)
    .bind(i64::from(max))
    .fetch_optional(pool)
    .await?;
    row.map(|r| {
        let n: i64 = r.try_get("attempts")?;
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    })
    .transpose()
}

pub async fn clear_code(pool: &Pool, session_id: &str, verifier: &str) -> Result<(), DbError> {
    sqlx::query("DELETE FROM agent_verifier_codes WHERE session_id = ? AND verifier = ?")
        .bind(session_id)
        .bind(verifier)
        .execute(pool)
        .await?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Send,
    Lookup,
}

impl EventKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Send => "send",
            Self::Lookup => "lookup",
        }
    }
}

pub struct NewEvent<'a> {
    pub principal_id: &'a str,
    pub verifier: &'a str,
    pub kind: EventKind,
    pub session_id: &'a str,
    pub email_hash: Option<&'a str>,
    pub ip_hash: Option<&'a str>,
    pub at: Timestamp,
}

/// Record one counted event, and drop this agent's events no window can
/// count any more.
pub async fn record_event(pool: &Pool, e: &NewEvent<'_>) -> Result<(), DbError> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO agent_verifier_events
           (id, principal_id, verifier, kind, session_id, email_hash, ip_hash, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(e.principal_id)
    .bind(e.verifier)
    .bind(e.kind.as_str())
    .bind(e.session_id)
    .bind(e.email_hash)
    .bind(e.ip_hash)
    .bind(e.at.to_string())
    .execute(&mut *tx)
    .await?;
    let horizon = e.at.checked_sub(MAX_WINDOW).unwrap_or(e.at);
    sqlx::query("DELETE FROM agent_verifier_events WHERE principal_id = ? AND created_at < ?")
        .bind(e.principal_id)
        .bind(horizon.to_string())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Whose events a window counts.
#[derive(Debug, Clone, Copy)]
pub enum Counted<'a> {
    /// Every conversation of the agent, for one address.
    Email(&'a str),
    /// Every conversation of the agent, from one client IP.
    Ip(&'a str),
    /// One conversation.
    Session(&'a str),
}

/// When each `kind` event of `verifier` counted by `who` happened, since
/// `since`.
pub async fn event_times(
    pool: &Pool,
    principal_id: &str,
    verifier: &str,
    kind: EventKind,
    who: Counted<'_>,
    since: Timestamp,
) -> Result<Vec<Timestamp>, DbError> {
    let (column, value) = match who {
        Counted::Email(v) => ("email_hash", v),
        Counted::Ip(v) => ("ip_hash", v),
        Counted::Session(v) => ("session_id", v),
    };
    let rows = sqlx::query(&format!(
        "SELECT created_at FROM agent_verifier_events
          WHERE principal_id = ? AND verifier = ? AND kind = ? AND {column} = ?
            AND created_at >= ?"
    ))
    .bind(principal_id)
    .bind(verifier)
    .bind(kind.as_str())
    .bind(value)
    .bind(since.to_string())
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|r| super::parse_ts(r.try_get("created_at")?, "created_at"))
        .collect()
}

/// Accept host identity token `jti_hash` for this agent once. `false` means
/// it was accepted before and has not expired yet: a replay. Runs on `tx`,
/// the caller's transaction, so the `jti` is spent only if what the token
/// was accepted for is stored too.
pub async fn use_jti(
    tx: &mut sqlx::SqliteConnection,
    principal_id: &str,
    jti_hash: &str,
    expires_at: Timestamp,
    now: Timestamp,
) -> Result<bool, DbError> {
    sqlx::query("DELETE FROM agent_identity_jtis WHERE principal_id = ? AND expires_at < ?")
        .bind(principal_id)
        .bind(now.to_string())
        .execute(&mut *tx)
        .await?;
    let inserted = sqlx::query(
        "INSERT INTO agent_identity_jtis (principal_id, jti_hash, expires_at) VALUES (?, ?, ?)
         ON CONFLICT(principal_id, jti_hash) DO NOTHING",
    )
    .bind(principal_id)
    .bind(jti_hash)
    .bind(expires_at.to_string())
    .execute(&mut *tx)
    .await?
    .rows_affected();
    Ok(inserted == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiplane_core::server::db::open;
    use std::path::Path;

    async fn fresh() -> Pool {
        let pool = open(Path::new(":memory:")).await.unwrap();
        for sql in [
            "INSERT INTO users (id, email, created_at, updated_at)
             VALUES ('u1', 'u1@example.com', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            "INSERT INTO system_principals (id, name, display, created_by, created_at)
             VALUES ('a1', 'support', 'Support', 'u1', '2026-01-01T00:00:00Z')",
            "INSERT INTO chat_sessions (id, user_id, created_at, updated_at)
             VALUES ('s1', 'u1', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            "INSERT INTO chat_sessions (id, user_id, created_at, updated_at)
             VALUES ('s2', 'u1', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        ] {
            sqlx::query(sql).execute(&pool).await.unwrap();
        }
        pool
    }

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[tokio::test]
    async fn a_code_allows_exactly_max_attempts_and_a_new_code_resets_them() {
        let pool = fresh().await;
        let sent = at("2026-10-02T10:00:00Z");
        let until = at("2026-10-02T10:10:00Z");
        assert_eq!(take_attempt(&pool, "s1", "otp", 2).await.unwrap(), None);
        put_code(&pool, "s1", "otp", "h1", sent, until)
            .await
            .unwrap();
        assert_eq!(take_attempt(&pool, "s1", "otp", 2).await.unwrap(), Some(1));
        assert_eq!(take_attempt(&pool, "s1", "otp", 2).await.unwrap(), Some(2));
        assert_eq!(take_attempt(&pool, "s1", "otp", 2).await.unwrap(), None);
        assert_eq!(
            pending_code(&pool, "s1", "otp").await.unwrap(),
            Some(PendingCode {
                email_hash: "h1".into(),
                sent_at: sent,
                expires_at: until,
                attempts: 2
            })
        );

        put_code(&pool, "s1", "otp", "h2", sent, until)
            .await
            .unwrap();
        assert_eq!(take_attempt(&pool, "s1", "otp", 2).await.unwrap(), Some(1));
        clear_code(&pool, "s1", "otp").await.unwrap();
        assert_eq!(pending_code(&pool, "s1", "otp").await.unwrap(), None);
    }

    #[tokio::test]
    async fn events_are_counted_per_address_ip_and_conversation() {
        let pool = fresh().await;
        let event = |session: &'static str, email: &'static str, at: Timestamp| NewEvent {
            principal_id: "a1",
            verifier: "otp",
            kind: EventKind::Send,
            session_id: session,
            email_hash: Some(email),
            ip_hash: Some("ip1"),
            at,
        };
        record_event(&pool, &event("s1", "e1", at("2026-10-02T10:00:00Z")))
            .await
            .unwrap();
        record_event(&pool, &event("s2", "e1", at("2026-10-02T10:05:00Z")))
            .await
            .unwrap();
        record_event(&pool, &event("s2", "e2", at("2026-10-02T10:06:00Z")))
            .await
            .unwrap();
        let since = at("2026-10-02T09:00:00Z");
        let count = |who| {
            let pool = pool.clone();
            async move {
                event_times(&pool, "a1", "otp", EventKind::Send, who, since)
                    .await
                    .unwrap()
                    .len()
            }
        };
        assert_eq!(count(Counted::Email("e1")).await, 2);
        assert_eq!(count(Counted::Ip("ip1")).await, 3);
        assert_eq!(count(Counted::Session("s2")).await, 2);
        assert_eq!(
            event_times(
                &pool,
                "a1",
                "otp",
                EventKind::Lookup,
                Counted::Ip("ip1"),
                since
            )
            .await
            .unwrap()
            .len(),
            0
        );

        record_event(&pool, &event("s1", "e3", at("2026-10-04T10:00:00Z")))
            .await
            .unwrap();
        assert_eq!(
            count(Counted::Ip("ip1")).await,
            1,
            "events older than any window are pruned"
        );
    }

    #[tokio::test]
    async fn a_jti_is_accepted_once_until_it_expires() {
        let pool = fresh().await;
        let now = at("2026-10-02T10:00:00Z");
        let exp = at("2026-10-02T10:05:00Z");
        let mut conn = pool.acquire().await.unwrap();
        assert!(use_jti(&mut conn, "a1", "j1", exp, now).await.unwrap());
        assert!(!use_jti(&mut conn, "a1", "j1", exp, now).await.unwrap());
        assert!(use_jti(&mut conn, "a1", "j2", exp, now).await.unwrap());
        let later = at("2026-10-02T10:06:00Z");
        assert!(
            use_jti(&mut conn, "a1", "j1", at("2026-10-02T10:11:00Z"), later)
                .await
                .unwrap(),
            "an expired entry no longer blocks; the token itself is expired by then"
        );
    }

    #[tokio::test]
    async fn a_jti_spent_in_a_rolled_back_transaction_is_still_unspent() {
        let pool = fresh().await;
        let now = at("2026-10-02T10:00:00Z");
        let exp = at("2026-10-02T10:05:00Z");
        let mut tx = pool.begin().await.unwrap();
        assert!(use_jti(&mut tx, "a1", "j1", exp, now).await.unwrap());
        tx.rollback().await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        assert!(use_jti(&mut tx, "a1", "j1", exp, now).await.unwrap());
        tx.commit().await.unwrap();
    }
}
