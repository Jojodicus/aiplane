// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Scheduled actions: per-user prompts that run automatically on a cron
//! schedule. Each fire opens a fresh chat session driven headlessly by
//! the same `OpenAiDriver` the interactive `/chat` path uses, so a
//! scheduled run is indistinguishable from a normal conversation in the
//! UI once it lands.
//!
//! This module owns the persistence (`scheduled_actions` table + CRUD)
//! and the cron evaluator ([`cron`]); the background loop that fires due
//! actions lives in [`worker`], and the web UI in
//! `rama_server::pages::scheduled`. The table is created by migration
//! `0021_scheduled_actions.sql`.

pub mod cron;
pub mod worker;

use jiff::Timestamp;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;
use uuid::Uuid;

use aiplane_core::server::db::{DbError, Pool};

/// A persisted scheduled action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledAction {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub prompt: String,
    pub model: String,
    /// 5-field cron expression (the source of truth; the UI builder and
    /// the advanced field both write this).
    pub cron: String,
    /// IANA timezone the cron expression is evaluated in.
    pub timezone: String,
    pub tools_enabled: bool,
    /// `true` = each fire reuses the previous run's chat session
    /// (`last_session_id`) so the model sees prior runs as history; `false`
    /// = each fire opens a fresh session. First run (or a deleted prior
    /// session) falls back to a fresh session regardless.
    pub reuse_conversation: bool,
    /// When reusing, how many recent rounds (one round = the run's prompt +
    /// reply = 2 turns) of history to replay — caps unbounded growth.
    pub reuse_rounds: i64,
    /// `false` = paused; the worker ignores it and `next_run_at` is NULL.
    pub enabled: bool,
    /// Precomputed next fire time (UTC). NULL when paused or when the
    /// expression has no future occurrence.
    pub next_run_at: Option<Timestamp>,
    pub last_run_at: Option<Timestamp>,
    /// `"ok"` or `"error"` — the outcome of the most recent run.
    pub last_status: Option<String>,
    /// Chat session opened by the most recent run, for the "open" link.
    pub last_session_id: Option<String>,
    pub last_error: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// The validated, ready-to-insert fields for a new action. `next_run_at`
/// is computed by the caller from `cron` + `timezone`.
pub struct NewAction {
    pub user_id: String,
    pub name: String,
    pub prompt: String,
    pub model: String,
    pub cron: String,
    pub timezone: String,
    pub tools_enabled: bool,
    pub reuse_conversation: bool,
    pub reuse_rounds: i64,
    pub next_run_at: Option<Timestamp>,
}

/// The mutable fields of an existing action, as submitted by the edit
/// form. `next_run_at` is recomputed by the caller from the new schedule.
pub struct EditAction {
    pub name: String,
    pub prompt: String,
    pub model: String,
    pub cron: String,
    pub timezone: String,
    pub tools_enabled: bool,
    pub reuse_conversation: bool,
    pub reuse_rounds: i64,
    pub next_run_at: Option<Timestamp>,
}

fn parse_ts(s: String, column: &'static str) -> Result<Timestamp, DbError> {
    s.parse().map_err(|e: jiff::Error| DbError::Decode {
        column,
        source: e.into(),
    })
}

fn parse_opt_ts(s: Option<String>, column: &'static str) -> Result<Option<Timestamp>, DbError> {
    s.map(|s| parse_ts(s, column)).transpose()
}

fn map_row(row: &SqliteRow) -> Result<ScheduledAction, DbError> {
    Ok(ScheduledAction {
        id: row.try_get("id")?,
        user_id: row.try_get("user_id")?,
        name: row.try_get("name")?,
        prompt: row.try_get("prompt")?,
        model: row.try_get("model")?,
        cron: row.try_get("cron")?,
        timezone: row.try_get("timezone")?,
        tools_enabled: row.try_get::<i64, _>("tools_enabled")? != 0,
        reuse_conversation: row.try_get::<i64, _>("reuse_conversation")? != 0,
        reuse_rounds: row.try_get("reuse_rounds")?,
        enabled: row.try_get::<i64, _>("enabled")? != 0,
        next_run_at: parse_opt_ts(row.try_get("next_run_at")?, "next_run_at")?,
        last_run_at: parse_opt_ts(row.try_get("last_run_at")?, "last_run_at")?,
        last_status: row.try_get("last_status")?,
        last_session_id: row.try_get("last_session_id")?,
        last_error: row.try_get("last_error")?,
        created_at: parse_ts(row.try_get("created_at")?, "created_at")?,
        updated_at: parse_ts(row.try_get("updated_at")?, "updated_at")?,
    })
}

const COLS: &str = "id, user_id, name, prompt, model, cron, timezone, tools_enabled, \
     reuse_conversation, reuse_rounds, enabled, next_run_at, last_run_at, last_status, \
     last_session_id, last_error, created_at, updated_at";

/// Insert a new action. Returns the stored row.
pub async fn create(pool: &Pool, new: NewAction) -> Result<ScheduledAction, DbError> {
    let now = Timestamp::now();
    let row = ScheduledAction {
        id: Uuid::new_v4().to_string(),
        user_id: new.user_id,
        name: new.name,
        prompt: new.prompt,
        model: new.model,
        cron: new.cron,
        timezone: new.timezone,
        tools_enabled: new.tools_enabled,
        reuse_conversation: new.reuse_conversation,
        reuse_rounds: new.reuse_rounds,
        enabled: true,
        next_run_at: new.next_run_at,
        last_run_at: None,
        last_status: None,
        last_session_id: None,
        last_error: None,
        created_at: now,
        updated_at: now,
    };
    sqlx::query(
        r#"INSERT INTO scheduled_actions
              (id, user_id, name, prompt, model, cron, timezone, tools_enabled,
               reuse_conversation, reuse_rounds, enabled, next_run_at, created_at, updated_at)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?)"#,
    )
    .bind(&row.id)
    .bind(&row.user_id)
    .bind(&row.name)
    .bind(&row.prompt)
    .bind(&row.model)
    .bind(&row.cron)
    .bind(&row.timezone)
    .bind(row.tools_enabled as i64)
    .bind(row.reuse_conversation as i64)
    .bind(row.reuse_rounds)
    .bind(row.next_run_at.map(|t| t.to_string()))
    .bind(row.created_at.to_string())
    .bind(row.updated_at.to_string())
    .execute(pool)
    .await?;
    Ok(row)
}

/// All of a user's actions, newest first.
pub async fn list_for_user(pool: &Pool, user_id: &str) -> Result<Vec<ScheduledAction>, DbError> {
    let sql = format!(
        "SELECT {COLS} FROM scheduled_actions WHERE user_id = ? \
         ORDER BY created_at DESC, id ASC"
    );
    let rows = sqlx::query(&sql).bind(user_id).fetch_all(pool).await?;
    rows.iter().map(map_row).collect()
}

/// One action, scoped to its owner (so a user can't read another's).
pub async fn get(pool: &Pool, user_id: &str, id: &str) -> Result<Option<ScheduledAction>, DbError> {
    let sql = format!("SELECT {COLS} FROM scheduled_actions WHERE id = ? AND user_id = ?");
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(user_id)
        .fetch_optional(pool)
        .await?;
    row.as_ref().map(map_row).transpose()
}

/// Apply an edit, scoped to the owner. Returns `true` if a row matched.
pub async fn update(
    pool: &Pool,
    user_id: &str,
    id: &str,
    edit: EditAction,
) -> Result<bool, DbError> {
    let affected = sqlx::query(
        r#"UPDATE scheduled_actions
           SET name = ?, prompt = ?, model = ?, cron = ?, timezone = ?,
               tools_enabled = ?, reuse_conversation = ?, reuse_rounds = ?,
               next_run_at = ?, updated_at = ?
           WHERE id = ? AND user_id = ?"#,
    )
    .bind(&edit.name)
    .bind(&edit.prompt)
    .bind(&edit.model)
    .bind(&edit.cron)
    .bind(&edit.timezone)
    .bind(edit.tools_enabled as i64)
    .bind(edit.reuse_conversation as i64)
    .bind(edit.reuse_rounds)
    .bind(edit.next_run_at.map(|t| t.to_string()))
    .bind(Timestamp::now().to_string())
    .bind(id)
    .bind(user_id)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

/// Pause or resume an action. When resuming, the caller passes the
/// freshly-computed `next_run_at`; when pausing it passes `None`, which
/// takes the row out of the worker's due query.
pub async fn set_enabled(
    pool: &Pool,
    user_id: &str,
    id: &str,
    enabled: bool,
    next_run_at: Option<Timestamp>,
) -> Result<bool, DbError> {
    let affected = sqlx::query(
        r#"UPDATE scheduled_actions
           SET enabled = ?, next_run_at = ?, updated_at = ?
           WHERE id = ? AND user_id = ?"#,
    )
    .bind(enabled as i64)
    .bind(next_run_at.map(|t| t.to_string()))
    .bind(Timestamp::now().to_string())
    .bind(id)
    .bind(user_id)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

/// Point a reusing schedule at the chat its next run continues in — any
/// existing conversation of the owner's, not only one a previous run opened —
/// or, with `None`, let the next run open a fresh one. The caller checks the
/// chat is the owner's. Owner-scoped; `false` if the action is not theirs.
pub async fn set_linked_session(
    pool: &Pool,
    user_id: &str,
    id: &str,
    session_id: Option<&str>,
) -> Result<bool, DbError> {
    let affected = sqlx::query(
        "UPDATE scheduled_actions SET last_session_id = ?, updated_at = ? \
         WHERE id = ? AND user_id = ?",
    )
    .bind(session_id)
    .bind(Timestamp::now().to_string())
    .bind(id)
    .bind(user_id)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

/// Delete an action, scoped to the owner. Returns `true` if a row matched.
pub async fn delete(pool: &Pool, user_id: &str, id: &str) -> Result<bool, DbError> {
    let affected = sqlx::query("DELETE FROM scheduled_actions WHERE id = ? AND user_id = ?")
        .bind(id)
        .bind(user_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(affected > 0)
}

/// Every enabled action whose `next_run_at` is at or before `now` —
/// i.e. due to fire. The `now` bind is floored to whole seconds so it
/// shares the canonical `…:SSZ` RFC3339 format the minute-aligned
/// `next_run_at` uses, keeping the string comparison chronologically
/// exact (no sub-second drift across the boundary).
pub async fn due_actions(pool: &Pool, now: Timestamp) -> Result<Vec<ScheduledAction>, DbError> {
    let now_floored = now.strftime("%Y-%m-%dT%H:%M:%SZ").to_string();
    let sql = format!(
        "SELECT {COLS} FROM scheduled_actions \
         WHERE enabled = 1 AND next_run_at IS NOT NULL AND next_run_at <= ? \
         ORDER BY next_run_at ASC"
    );
    let rows = sqlx::query(&sql).bind(now_floored).fetch_all(pool).await?;
    rows.iter().map(map_row).collect()
}

/// Advance only `next_run_at`. The worker "claims" a due action by
/// pushing this to the next future occurrence *before* it runs, so a
/// slow run (longer than the poll interval) or a crash mid-run can never
/// fire the same occurrence twice.
pub async fn set_next_run(
    pool: &Pool,
    id: &str,
    next_run_at: Option<Timestamp>,
) -> Result<(), DbError> {
    sqlx::query("UPDATE scheduled_actions SET next_run_at = ?, updated_at = ? WHERE id = ?")
        .bind(next_run_at.map(|t| t.to_string()))
        .bind(Timestamp::now().to_string())
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Record the outcome of a run and advance `next_run_at`. `status` is
/// `"ok"` or `"error"`; `session_id` is the chat the run opened (kept
/// even on error so the user can inspect the partial conversation). `None`
/// keeps the chat already on record: a fire that opened none (skipped over
/// quota) must not cut a reusing schedule off from its conversation.
pub async fn mark_ran(
    pool: &Pool,
    id: &str,
    status: &str,
    session_id: Option<&str>,
    next_run_at: Option<Timestamp>,
    error: Option<&str>,
) -> Result<(), DbError> {
    sqlx::query(
        r#"UPDATE scheduled_actions
           SET last_run_at = ?, last_status = ?, last_session_id = COALESCE(?, last_session_id),
               last_error = ?, next_run_at = ?, updated_at = ?
           WHERE id = ?"#,
    )
    .bind(Timestamp::now().to_string())
    .bind(status)
    .bind(session_id)
    .bind(error)
    .bind(next_run_at.map(|t| t.to_string()))
    .bind(Timestamp::now().to_string())
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Run history (scheduled_runs, migration 0064)

/// One recorded fire of a scheduled action.
///
/// `session_id` is the chat the run opened — the link the user comes to
/// `/scheduled` for. An action with `reuse_conversation` points every run at
/// the same session by design; the runs still list separately, because when
/// each fired and whether it worked differ even when the conversation does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledRun {
    pub id: String,
    pub action_id: String,
    pub fired_at: Timestamp,
    /// `None` only while the run is in flight; `"ok"` or `"error"` once done.
    pub status: Option<String>,
    pub session_id: Option<String>,
    /// The run opened a chat that the user has since deleted. Kept apart from
    /// `session_id = None` (the run never opened one) so the history can say
    /// which of the two happened.
    pub chat_deleted: bool,
    pub error: Option<String>,
    pub created_at: Timestamp,
}

const RUN_COLS: &str = "r.id, r.action_id, r.fired_at, r.status, r.session_id, r.error, r.created_at, \
     (r.session_id IS NOT NULL AND s.id IS NULL) AS chat_deleted";

/// What a run left pending by a dead process is closed with at startup.
pub const RUN_INTERRUPTED: &str = "interrupted — the server stopped before this run finished";

fn map_run(row: &SqliteRow) -> Result<ScheduledRun, DbError> {
    Ok(ScheduledRun {
        id: row.try_get("id")?,
        action_id: row.try_get("action_id")?,
        fired_at: parse_ts(row.try_get("fired_at")?, "fired_at")?,
        status: row.try_get("status")?,
        session_id: row.try_get("session_id")?,
        chat_deleted: row.try_get("chat_deleted")?,
        error: row.try_get("error")?,
        created_at: parse_ts(row.try_get("created_at")?, "created_at")?,
    })
}

/// Record the start of a run (status left NULL until [`finish_run`]). Returns
/// the new run id. Not owner-scoped: the worker has already loaded the action.
pub async fn record_run_start(pool: &Pool, action_id: &str) -> Result<String, DbError> {
    let id = Uuid::new_v4().to_string();
    let now = Timestamp::now().to_string();
    sqlx::query(
        r#"INSERT INTO scheduled_runs (id, action_id, fired_at, created_at)
           VALUES (?, ?, ?, ?)"#,
    )
    .bind(&id)
    .bind(action_id)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(id)
}

/// Link a pending run to the chat it just opened, ahead of its outcome, so a
/// run that dies mid-turn still points at the conversation it was writing.
/// The action's `last_session_id` moves with it: a reusing schedule continues
/// in that chat next time even if this run never finishes.
pub async fn attach_run_session(
    pool: &Pool,
    run_id: &str,
    session_id: &str,
) -> Result<(), DbError> {
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE scheduled_runs SET session_id = ? WHERE id = ?")
        .bind(session_id)
        .bind(run_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE scheduled_actions SET last_session_id = ? \
         WHERE id = (SELECT action_id FROM scheduled_runs WHERE id = ?)",
    )
    .bind(session_id)
    .bind(run_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Close every run still pending at startup: no worker survives a restart, so
/// nothing will ever finish them, and left alone they read as running forever.
/// An action whose newest run is one of them gets the same outcome on its list
/// row, which would otherwise still describe the run before. Returns how many
/// runs were closed.
///
/// Only sound before the scheduler starts firing: a run pending *then* is
/// necessarily a previous process's.
pub async fn sweep_interrupted_runs(pool: &Pool) -> Result<u64, DbError> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        r#"UPDATE scheduled_actions
           SET last_status = 'error', last_error = ?1, last_run_at = newest.fired_at
           FROM (SELECT r.action_id, r.fired_at
                 FROM scheduled_runs r
                 WHERE r.status IS NULL
                   AND r.rowid = (SELECT rowid FROM scheduled_runs
                                  WHERE action_id = r.action_id
                                  ORDER BY fired_at DESC, rowid DESC LIMIT 1)) AS newest
           WHERE scheduled_actions.id = newest.action_id"#,
    )
    .bind(RUN_INTERRUPTED)
    .execute(&mut *tx)
    .await?;
    let done =
        sqlx::query("UPDATE scheduled_runs SET status = 'error', error = ? WHERE status IS NULL")
            .bind(RUN_INTERRUPTED)
            .execute(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(done.rows_affected())
}

/// Record the outcome of a run started by [`record_run_start`]. `session_id`
/// is kept even on error so the user can inspect the partial conversation;
/// `None` keeps the one [`attach_run_session`] already linked.
pub async fn finish_run(
    pool: &Pool,
    run_id: &str,
    status: &str,
    session_id: Option<&str>,
    error: Option<&str>,
) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE scheduled_runs SET status = ?, session_id = COALESCE(?, session_id), error = ? \
         WHERE id = ?",
    )
    .bind(status)
    .bind(session_id)
    .bind(error)
    .bind(run_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// An action's most recent runs, newest first, capped at `limit`.
///
/// The tiebreaker is `rowid DESC` (SQLite's monotonic insertion order), NOT
/// `id` — `id` is a random UUID, so two runs sharing a `fired_at` tick would
/// otherwise come back in nondeterministic order.
pub async fn list_runs(
    pool: &Pool,
    action_id: &str,
    limit: i64,
) -> Result<Vec<ScheduledRun>, DbError> {
    let sql = format!(
        "SELECT {RUN_COLS} FROM scheduled_runs r \
         LEFT JOIN chat_sessions s ON s.id = r.session_id \
         WHERE r.action_id = ? \
         ORDER BY r.fired_at DESC, r.rowid DESC LIMIT ?"
    );
    let rows = sqlx::query(&sql)
        .bind(action_id)
        .bind(limit)
        .fetch_all(pool)
        .await?;
    rows.iter().map(map_run).collect()
}

/// How many runs, and how many distinct chats, each of a user's actions has.
///
/// Both numbers in one grouped query rather than one per row: the list page
/// renders every action, and the two differ for a `reuse_conversation`
/// schedule (many runs, one chat) — which is exactly the distinction the
/// row's link has to make.
pub async fn run_counts_for_user(
    pool: &Pool,
    user_id: &str,
) -> Result<std::collections::HashMap<String, (i64, i64)>, DbError> {
    let rows = sqlx::query(
        r#"SELECT r.action_id AS action_id,
                  COUNT(*) AS runs,
                  COUNT(DISTINCT r.session_id) AS chats
           FROM scheduled_runs r
           JOIN scheduled_actions a ON a.id = r.action_id
           WHERE a.user_id = ?
           GROUP BY r.action_id"#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            Ok((
                row.try_get::<String, _>("action_id")?,
                (row.try_get("runs")?, row.try_get("chats")?),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fresh_db() -> Pool {
        // `open` runs the migration set, which includes 0021 (the
        // scheduled_actions table).
        aiplane_core::server::db::open(std::path::Path::new(":memory:"))
            .await
            .unwrap()
    }

    async fn seed_user(pool: &Pool, id: &str) {
        // The FK to users(id) means we need a row to attach actions to.
        let now = Timestamp::now();
        aiplane_core::server::db::users::upsert(
            pool,
            &aiplane_core::server::db::users::User {
                id: id.to_string(),
                email: format!("{id}@example.com"),
                name: None,
                roles: vec![],
                created_at: now,
                updated_at: now,
                timezone: Some("Europe/Berlin".to_string()),
                speech_voice: None,
            },
        )
        .await
        .unwrap();
    }

    fn sample(user_id: &str, next: Option<Timestamp>) -> NewAction {
        NewAction {
            user_id: user_id.to_string(),
            name: "Daily digest".to_string(),
            prompt: "Summarize the news.".to_string(),
            model: "qwen".to_string(),
            cron: "0 9 * * *".to_string(),
            timezone: "Europe/Berlin".to_string(),
            tools_enabled: true,
            reuse_conversation: false,
            reuse_rounds: 5,
            next_run_at: next,
        }
    }

    #[tokio::test]
    async fn create_then_get_round_trips() {
        let pool = fresh_db().await;
        seed_user(&pool, "u1").await;
        let next = "2026-06-19T07:00:00Z".parse::<Timestamp>().unwrap();
        let created = create(&pool, sample("u1", Some(next))).await.unwrap();
        let got = get(&pool, "u1", &created.id).await.unwrap().unwrap();
        assert_eq!(got, created);
        assert_eq!(got.next_run_at, Some(next));
        assert!(got.enabled && got.tools_enabled);
    }

    #[tokio::test]
    async fn get_is_scoped_to_owner() {
        let pool = fresh_db().await;
        seed_user(&pool, "u1").await;
        seed_user(&pool, "u2").await;
        let created = create(&pool, sample("u1", None)).await.unwrap();
        assert!(get(&pool, "u2", &created.id).await.unwrap().is_none());
        assert!(!delete(&pool, "u2", &created.id).await.unwrap());
        assert!(get(&pool, "u1", &created.id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn due_actions_selects_only_enabled_and_due() {
        let pool = fresh_db().await;
        seed_user(&pool, "u1").await;
        let past = "2026-06-19T06:00:00Z".parse::<Timestamp>().unwrap();
        let future = "2999-01-01T00:00:00Z".parse::<Timestamp>().unwrap();
        let due = create(&pool, sample("u1", Some(past))).await.unwrap();
        let not_yet = create(&pool, sample("u1", Some(future))).await.unwrap();
        let paused = create(&pool, sample("u1", Some(past))).await.unwrap();
        set_enabled(&pool, "u1", &paused.id, false, None)
            .await
            .unwrap();

        let now = "2026-06-19T06:30:00Z".parse::<Timestamp>().unwrap();
        let ids: Vec<String> = due_actions(&pool, now)
            .await
            .unwrap()
            .into_iter()
            .map(|a| a.id)
            .collect();
        assert_eq!(ids, vec![due.id.clone()]);
        assert!(!ids.contains(&not_yet.id));
        assert!(!ids.contains(&paused.id));
    }

    #[tokio::test]
    async fn mark_ran_advances_next_run_and_records_status() {
        let pool = fresh_db().await;
        seed_user(&pool, "u1").await;
        let past = "2026-06-19T06:00:00Z".parse::<Timestamp>().unwrap();
        let a = create(&pool, sample("u1", Some(past))).await.unwrap();
        let advanced = "2026-06-20T07:00:00Z".parse::<Timestamp>().unwrap();
        mark_ran(&pool, &a.id, "ok", Some("sess-1"), Some(advanced), None)
            .await
            .unwrap();
        let got = get(&pool, "u1", &a.id).await.unwrap().unwrap();
        assert_eq!(got.next_run_at, Some(advanced));
        assert_eq!(got.last_status.as_deref(), Some("ok"));
        assert_eq!(got.last_session_id.as_deref(), Some("sess-1"));
        assert!(got.last_run_at.is_some());
    }

    #[tokio::test]
    async fn runs_record_start_then_outcome_newest_first() {
        let pool = fresh_db().await;
        seed_user(&pool, "u1").await;
        let a = create(&pool, sample("u1", None)).await.unwrap();

        let first = record_run_start(&pool, &a.id).await.unwrap();
        // A run is visible the moment it starts, pending, so a long run
        // doesn't look like it never happened.
        let pending = list_runs(&pool, &a.id, 50).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].status, None);
        assert_eq!(pending[0].session_id, None);

        finish_run(&pool, &first, "ok", Some("sess-1"), None)
            .await
            .unwrap();
        let second = record_run_start(&pool, &a.id).await.unwrap();
        finish_run(&pool, &second, "error", Some("sess-2"), Some("boom"))
            .await
            .unwrap();

        let runs = list_runs(&pool, &a.id, 50).await.unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].id, second);
        assert_eq!(runs[0].status.as_deref(), Some("error"));
        assert_eq!(runs[0].error.as_deref(), Some("boom"));
        // The session is kept on a failed run: the partial conversation is
        // usually where the reason is.
        assert_eq!(runs[0].session_id.as_deref(), Some("sess-2"));
        assert_eq!(runs[1].id, first);
        assert_eq!(runs[1].status.as_deref(), Some("ok"));
        assert_eq!(list_runs(&pool, &a.id, 1).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn run_counts_separate_runs_from_chats() {
        let pool = fresh_db().await;
        seed_user(&pool, "u1").await;
        seed_user(&pool, "u2").await;
        let fresh_each_time = create(&pool, sample("u1", None)).await.unwrap();
        let reuses = create(&pool, sample("u1", None)).await.unwrap();
        let other_owner = create(&pool, sample("u2", None)).await.unwrap();

        for session in ["sess-a", "sess-b"] {
            let run = record_run_start(&pool, &fresh_each_time.id).await.unwrap();
            finish_run(&pool, &run, "ok", Some(session), None)
                .await
                .unwrap();
        }
        // A reusing schedule fires repeatedly into one conversation: three
        // runs, one chat. That is the distinction the list row's link makes.
        for _ in 0..3 {
            let run = record_run_start(&pool, &reuses.id).await.unwrap();
            finish_run(&pool, &run, "ok", Some("sess-shared"), None)
                .await
                .unwrap();
        }
        let run = record_run_start(&pool, &other_owner.id).await.unwrap();
        finish_run(&pool, &run, "ok", Some("sess-other"), None)
            .await
            .unwrap();

        let counts = run_counts_for_user(&pool, "u1").await.unwrap();
        assert_eq!(counts.get(&fresh_each_time.id), Some(&(2, 2)));
        assert_eq!(counts.get(&reuses.id), Some(&(3, 1)));
        // Another user's action never appears in this user's counts.
        assert_eq!(counts.get(&other_owner.id), None);
    }

    /// The run links its chat the moment the session is opened, not when it
    /// finishes: a run that never reaches `finish_run` (a panic, a restart)
    /// must still point at the conversation it was writing.
    #[tokio::test]
    async fn a_pending_run_links_its_chat_as_soon_as_the_session_opens() {
        let pool = fresh_db().await;
        seed_user(&pool, "u1").await;
        let a = create(&pool, sample("u1", None)).await.unwrap();
        let run = record_run_start(&pool, &a.id).await.unwrap();

        attach_run_session(&pool, &run, "sess-1").await.unwrap();

        let runs = list_runs(&pool, &a.id, 50).await.unwrap();
        assert_eq!(runs[0].status, None);
        assert_eq!(runs[0].session_id.as_deref(), Some("sess-1"));
        // A reusing schedule continues in this chat even if the run dies here.
        let a = get(&pool, "u1", &a.id).await.unwrap().unwrap();
        assert_eq!(a.last_session_id.as_deref(), Some("sess-1"));

        // Closing the run without a session (it crashed before it knew one)
        // keeps the link rather than erasing it.
        finish_run(&pool, &run, "error", None, Some("crashed"))
            .await
            .unwrap();
        mark_ran(&pool, &a.id, "error", None, None, Some("crashed"))
            .await
            .unwrap();
        let runs = list_runs(&pool, &a.id, 50).await.unwrap();
        assert_eq!(runs[0].session_id.as_deref(), Some("sess-1"));
        let a = get(&pool, "u1", &a.id).await.unwrap().unwrap();
        assert_eq!(a.last_session_id.as_deref(), Some("sess-1"));
    }

    #[tokio::test]
    async fn list_runs_reports_a_deleted_chat() {
        let pool = fresh_db().await;
        seed_user(&pool, "u1").await;
        let a = create(&pool, sample("u1", None)).await.unwrap();
        let kept = session_core::db::create_session(&pool, "u1").await.unwrap();
        let deleted = session_core::db::create_session(&pool, "u1").await.unwrap();
        for session in [&kept.id, &deleted.id] {
            let run = record_run_start(&pool, &a.id).await.unwrap();
            finish_run(&pool, &run, "ok", Some(session), None)
                .await
                .unwrap();
        }
        let no_chat = record_run_start(&pool, &a.id).await.unwrap();
        finish_run(&pool, &no_chat, "error", None, Some("usage limit"))
            .await
            .unwrap();
        assert!(
            session_core::db::delete_session(&pool, "u1", &deleted.id)
                .await
                .unwrap()
        );

        let runs = list_runs(&pool, &a.id, 50).await.unwrap();
        let by_session = |id: Option<&str>| {
            runs.iter()
                .find(|r| r.session_id.as_deref() == id)
                .unwrap()
                .chat_deleted
        };
        assert!(!by_session(Some(&kept.id)));
        assert!(by_session(Some(&deleted.id)));
        // A run that never opened a chat has nothing that could be deleted.
        assert!(!by_session(None));
    }

    #[tokio::test]
    async fn startup_sweep_closes_runs_a_dead_process_left_pending() {
        let pool = fresh_db().await;
        seed_user(&pool, "u1").await;
        let a = create(&pool, sample("u1", None)).await.unwrap();
        let finished = record_run_start(&pool, &a.id).await.unwrap();
        finish_run(&pool, &finished, "ok", Some("sess-1"), None)
            .await
            .unwrap();
        mark_ran(&pool, &a.id, "ok", Some("sess-1"), None, None)
            .await
            .unwrap();
        let orphan = record_run_start(&pool, &a.id).await.unwrap();
        attach_run_session(&pool, &orphan, "sess-2").await.unwrap();
        // An older orphan on an action whose later run finished: that list row
        // describes the later run and must stay as it is.
        let b = create(&pool, sample("u1", None)).await.unwrap();
        let old_orphan = record_run_start(&pool, &b.id).await.unwrap();
        let later = record_run_start(&pool, &b.id).await.unwrap();
        finish_run(&pool, &later, "ok", Some("sess-b"), None)
            .await
            .unwrap();
        mark_ran(&pool, &b.id, "ok", Some("sess-b"), None, None)
            .await
            .unwrap();

        assert_eq!(sweep_interrupted_runs(&pool).await.unwrap(), 2);

        let runs = list_runs(&pool, &a.id, 50).await.unwrap();
        let orphan_run = runs.iter().find(|r| r.id == orphan).unwrap();
        assert_eq!(orphan_run.status.as_deref(), Some("error"));
        assert_eq!(orphan_run.error.as_deref(), Some(RUN_INTERRUPTED));
        assert_eq!(orphan_run.session_id.as_deref(), Some("sess-2"));
        let finished = runs.iter().find(|r| r.id == finished).unwrap();
        assert_eq!(finished.status.as_deref(), Some("ok"));
        assert_eq!(finished.error, None);
        let a_row = get(&pool, "u1", &a.id).await.unwrap().unwrap();
        assert_eq!(a_row.last_status.as_deref(), Some("error"));
        assert_eq!(a_row.last_error.as_deref(), Some(RUN_INTERRUPTED));
        assert_eq!(a_row.last_session_id.as_deref(), Some("sess-2"));
        assert_eq!(a_row.last_run_at, Some(orphan_run.fired_at));

        let b_row = get(&pool, "u1", &b.id).await.unwrap().unwrap();
        assert_eq!(b_row.last_status.as_deref(), Some("ok"));
        assert_eq!(b_row.last_error, None);
        let b_runs = list_runs(&pool, &b.id, 50).await.unwrap();
        let old_orphan = b_runs.iter().find(|r| r.id == old_orphan).unwrap();
        assert_eq!(old_orphan.status.as_deref(), Some("error"));

        assert_eq!(
            sweep_interrupted_runs(&pool).await.unwrap(),
            0,
            "a second pass finds nothing left to close"
        );
    }

    #[tokio::test]
    async fn a_linked_chat_is_where_the_next_run_continues() {
        let pool = fresh_db().await;
        seed_user(&pool, "u1").await;
        seed_user(&pool, "u2").await;
        let a = create(&pool, sample("u1", None)).await.unwrap();

        assert!(
            set_linked_session(&pool, "u1", &a.id, Some("chat-1"))
                .await
                .unwrap()
        );
        assert_eq!(
            get(&pool, "u1", &a.id)
                .await
                .unwrap()
                .unwrap()
                .last_session_id
                .as_deref(),
            Some("chat-1")
        );

        assert!(set_linked_session(&pool, "u1", &a.id, None).await.unwrap());
        assert_eq!(
            get(&pool, "u1", &a.id)
                .await
                .unwrap()
                .unwrap()
                .last_session_id,
            None
        );

        assert!(
            !set_linked_session(&pool, "u2", &a.id, Some("chat-2"))
                .await
                .unwrap(),
            "another user cannot repoint the action"
        );
        assert_eq!(
            get(&pool, "u1", &a.id)
                .await
                .unwrap()
                .unwrap()
                .last_session_id,
            None
        );
    }

    #[tokio::test]
    async fn deleting_an_action_deletes_its_runs() {
        let pool = fresh_db().await;
        seed_user(&pool, "u1").await;
        let a = create(&pool, sample("u1", None)).await.unwrap();
        let run = record_run_start(&pool, &a.id).await.unwrap();
        finish_run(&pool, &run, "ok", Some("sess-1"), None)
            .await
            .unwrap();
        assert!(delete(&pool, "u1", &a.id).await.unwrap());
        assert!(list_runs(&pool, &a.id, 50).await.unwrap().is_empty());
    }
}
