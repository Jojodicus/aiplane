// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

use super::*;

/// Create a freshly empty conversation for `user_id`. Returns the new
/// row; the caller's next step is usually to redirect to its URL.
pub async fn create_session(pool: &Pool, user_id: &str) -> Result<Session, DbError> {
    let now = Timestamp::now();
    let s = Session {
        id: Uuid::new_v4().to_string(),
        user_id: user_id.to_string(),
        title: None,
        created_at: now,
        updated_at: now,
        shared: false,
        pinned: false,
    };
    sqlx::query(
        r#"INSERT INTO chat_sessions (id, user_id, title, created_at, updated_at)
           VALUES (?, ?, ?, ?, ?)"#,
    )
    .bind(&s.id)
    .bind(&s.user_id)
    .bind(s.title.as_deref())
    .bind(s.created_at.to_string())
    .bind(s.updated_at.to_string())
    .execute(pool)
    .await?;
    Ok(s)
}

/// All conversations for a user. Pinned conversations float to the top
/// (in their own recency order); the rest follow, also most-recent first.
pub async fn list_sessions(pool: &Pool, user_id: &str) -> Result<Vec<Session>, DbError> {
    let rows = sqlx::query(
        r#"SELECT id, user_id, title, created_at, updated_at, shared, pinned
           FROM chat_sessions
           WHERE user_id = ?
           ORDER BY pinned DESC, updated_at DESC, id ASC"#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    rows.iter().map(map_session).collect()
}

/// Look up a session by id, but only if it belongs to this user. Caller
/// uses the None case to send a 404 / redirect to /chat.
pub async fn get_session(
    pool: &Pool,
    user_id: &str,
    session_id: &str,
) -> Result<Option<Session>, DbError> {
    let row = sqlx::query(
        r#"SELECT id, user_id, title, created_at, updated_at, shared, pinned
           FROM chat_sessions
           WHERE id = ? AND user_id = ?"#,
    )
    .bind(session_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(map_session).transpose()
}

/// Owner of the chat session a given turn belongs to. Used by the
/// attachment-proxy route to authorize `GET /chat/attachment/<turn>/
/// <file>`: the caller's session must match the returned `user_id`,
/// otherwise user A could fetch user B's uploaded files by guessing
/// turn ids. `None` when the turn id doesn't exist, and for a turn of a
/// conversation no person owns (`user_id` NULL).
pub async fn user_for_turn(pool: &Pool, turn_id: &str) -> Result<Option<String>, DbError> {
    let row = sqlx::query(
        r#"SELECT s.user_id AS user_id
           FROM chat_turns t
           JOIN chat_sessions s ON s.id = t.session_id
           WHERE t.id = ?"#,
    )
    .bind(turn_id)
    .fetch_optional(pool)
    .await?;
    Ok(row
        .map(|r| r.try_get::<Option<String>, _>("user_id"))
        .transpose()?
        .flatten())
}

/// Look up a session readable by `viewer_id`: either they own it, or it has
/// been shared (`shared = 1`). Used by the read-only paths (view, tail,
/// attachments) where any signed-in user holding the session's UUID may read
/// a shared conversation. Mutating paths must keep using the owner-only
/// [`get_session`].
pub async fn get_session_readable(
    pool: &Pool,
    viewer_id: &str,
    session_id: &str,
) -> Result<Option<Session>, DbError> {
    let row = sqlx::query(
        r#"SELECT id, user_id, title, created_at, updated_at, shared, pinned
           FROM chat_sessions
           WHERE id = ? AND (user_id = ? OR shared = 1)"#,
    )
    .bind(session_id)
    .bind(viewer_id)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(map_session).transpose()
}

/// Set (or clear) a session's shared flag — owner-only. Returns true when a
/// row was updated (the caller owns it); false otherwise, so a non-owner's
/// attempt is a silent no-op rather than leaking existence via an error.
pub async fn set_shared(
    pool: &Pool,
    user_id: &str,
    session_id: &str,
    shared: bool,
) -> Result<bool, DbError> {
    let res = sqlx::query(r#"UPDATE chat_sessions SET shared = ? WHERE id = ? AND user_id = ?"#)
        .bind(shared as i64)
        .bind(session_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected() > 0)
}

/// Set (or clear) a session's pinned flag — owner-only. Returns true when a
/// row was updated (the caller owns it); false otherwise, so a non-owner's
/// attempt is a silent no-op rather than leaking existence via an error.
/// Same owner-scoping guarantee as [`set_shared`].
pub async fn set_pinned(
    pool: &Pool,
    user_id: &str,
    session_id: &str,
    pinned: bool,
) -> Result<bool, DbError> {
    let res = sqlx::query(r#"UPDATE chat_sessions SET pinned = ? WHERE id = ? AND user_id = ?"#)
        .bind(pinned as i64)
        .bind(session_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected() > 0)
}

/// Whether the chat session a given turn belongs to is readable by
/// `viewer_id` — owner or shared. Backs the attachment proxy so files in a
/// shared conversation are fetchable by a viewer, while a private turn's
/// files stay owner-only. `false` when the turn id doesn't exist.
pub async fn turn_session_readable(
    pool: &Pool,
    turn_id: &str,
    viewer_id: &str,
) -> Result<bool, DbError> {
    let row = sqlx::query(
        r#"SELECT 1 AS ok
           FROM chat_turns t
           JOIN chat_sessions s ON s.id = t.session_id
           WHERE t.id = ? AND (s.user_id = ? OR s.shared = 1)"#,
    )
    .bind(turn_id)
    .bind(viewer_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.is_some())
}

/// Whether `turn_id` is a turn of `session_id`. The session-scoping gate
/// for model-supplied `<turn_id>/<filename>` attachment ids that carry no
/// marker in the conversation text — a typst render's hidden `.json` edit
/// base, for instance — and so can't be proven in-session by
/// `chat_attachments::list_session_attachments`. `false` for an unknown
/// turn, so a guessed id is refused rather than fetched.
pub async fn turn_in_session(
    pool: &Pool,
    turn_id: &str,
    session_id: &str,
) -> Result<bool, DbError> {
    let row = sqlx::query(r#"SELECT 1 AS ok FROM chat_turns WHERE id = ? AND session_id = ?"#)
        .bind(turn_id)
        .bind(session_id)
        .fetch_optional(pool)
        .await?;
    Ok(row.is_some())
}

/// Most-recent session for a user; None when they've never chatted. Used
/// by `GET /chat` to decide where to redirect.
pub async fn latest_session(pool: &Pool, user_id: &str) -> Result<Option<Session>, DbError> {
    let row = sqlx::query(
        r#"SELECT id, user_id, title, created_at, updated_at, shared, pinned
           FROM chat_sessions
           WHERE user_id = ?
           ORDER BY updated_at DESC, id ASC
           LIMIT 1"#,
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(map_session).transpose()
}

/// Which of `ids` still name a session. Lets a page that holds chat ids it did
/// not create (a schedule's runs, a webhook's fires) tell a deleted chat apart
/// from a live one in one query rather than one per row.
pub async fn existing_session_ids(
    pool: &Pool,
    ids: &[&str],
) -> Result<std::collections::HashSet<String>, DbError> {
    if ids.is_empty() {
        return Ok(std::collections::HashSet::new());
    }
    let placeholders = vec!["?"; ids.len()].join(", ");
    let sql = format!("SELECT id FROM chat_sessions WHERE id IN ({placeholders})");
    let mut query = sqlx::query_scalar::<_, String>(&sql);
    for id in ids {
        query = query.bind(*id);
    }
    Ok(query.fetch_all(pool).await?.into_iter().collect())
}

/// Delete a session (cascades to turns + tool_calls). Returns true iff
/// a row was actually removed — caller uses this to send a clean toast
/// vs a "not found" one.
pub async fn delete_session(pool: &Pool, user_id: &str, session_id: &str) -> Result<bool, DbError> {
    let result = sqlx::query(r#"DELETE FROM chat_sessions WHERE id = ? AND user_id = ?"#)
        .bind(session_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::tests::pool;

    async fn completed_turn(pool: &Pool, session_id: &str, text: &str) -> String {
        let id = Uuid::new_v4().to_string();
        create_user_turn(pool, session_id, &id, text).await.unwrap();
        id
    }

    /// A conversation another owner holds (`user_id` NULL) is opaque here:
    /// no person's view lists, searches, opens or shares it.
    #[tokio::test]
    async fn a_conversation_without_a_person_is_in_no_persons_view() {
        let pool = pool().await;
        let mine = create_session(&pool, "u1").await.unwrap();
        set_session_title(&pool, &mine.id, "invoices for march")
            .await
            .unwrap();
        let other = crate::db::tests::unowned_session(&pool).await;
        let other_turn = completed_turn(&pool, &other, "invoices please").await;

        let listed: Vec<String> = list_sessions(&pool, "u1")
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(listed, vec![mine.id.clone()]);
        assert_eq!(
            latest_session(&pool, "u1").await.unwrap().map(|s| s.id),
            Some(mine.id.clone())
        );
        assert!(get_session(&pool, "u1", &other).await.unwrap().is_none());
        assert!(
            get_session_readable(&pool, "u1", &other)
                .await
                .unwrap()
                .is_none()
        );
        let hits = search_sessions(&pool, "u1", "invoices", 10).await.unwrap();
        assert_eq!(
            hits.iter()
                .map(|h| h.session_id.as_str())
                .collect::<Vec<_>>(),
            vec![mine.id.as_str()]
        );
        assert_eq!(user_for_turn(&pool, &other_turn).await.unwrap(), None);
        assert!(
            !turn_session_readable(&pool, &other_turn, "u1")
                .await
                .unwrap()
        );
        assert!(!set_shared(&pool, "u1", &other, true).await.unwrap());
        assert!(!delete_session(&pool, "u1", &other).await.unwrap());
    }
}
