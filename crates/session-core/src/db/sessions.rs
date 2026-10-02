// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

use super::*;
use crate::i18n::Lang;

/// Who a conversation belongs to: a person, or a system principal running it
/// (an agent run). Exactly one, enforced by a CHECK on `chat_sessions`.
///
/// Every person-facing read in this module filters on `user_id`, which is
/// `NULL` for a principal-owned run, so a run is never listed, searched,
/// opened or shared as anyone's chat — by construction, not by a guard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionOwner {
    User(String),
    Principal(String),
}

/// Inputs to [`create_principal_session`].
#[derive(Debug, Clone, Copy)]
pub struct NewRunSession<'a> {
    pub principal_id: &'a str,
    pub title: Option<&'a str>,
    /// On a sub-agent run: the calling agent's turn.
    pub parent_turn_id: Option<&'a str>,
    /// The agent version the run executes, when the principal is an agent.
    pub agent_version: Option<i64>,
}

/// A conversation owned by a system principal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSession {
    pub id: String,
    pub principal_id: String,
    pub parent_turn_id: Option<String>,
    pub agent_version: Option<i64>,
    /// The visitor session the conversation serves, on a public agent's.
    pub visitor_id: Option<String>,
    /// The language its latest turn was asked in ([`set_run_lang`]).
    pub lang: Option<Lang>,
    pub title: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// Open a conversation owned by a system principal. Fails when the principal
/// does not exist (foreign key). Takes any executor so a caller can open it
/// inside its own transaction (a visitor session links to it atomically).
pub async fn create_principal_session<'e, E>(
    exec: E,
    new: &NewRunSession<'_>,
) -> Result<RunSession, DbError>
where
    E: sqlx::SqliteExecutor<'e>,
{
    let now = Timestamp::now();
    let s = RunSession {
        id: Uuid::new_v4().to_string(),
        principal_id: new.principal_id.to_string(),
        parent_turn_id: new.parent_turn_id.map(str::to_string),
        agent_version: new.agent_version,
        visitor_id: None,
        lang: None,
        title: new.title.map(str::to_string),
        created_at: now,
        updated_at: now,
    };
    sqlx::query(
        r#"INSERT INTO chat_sessions
             (id, principal_id, parent_turn_id, agent_version, title, created_at, updated_at)
           VALUES (?, ?, ?, ?, ?, ?, ?)"#,
    )
    .bind(&s.id)
    .bind(&s.principal_id)
    .bind(s.parent_turn_id.as_deref())
    .bind(s.agent_version)
    .bind(s.title.as_deref())
    .bind(s.created_at.to_string())
    .bind(s.updated_at.to_string())
    .execute(exec)
    .await?;
    Ok(s)
}

/// A principal-owned conversation, only if `principal_id` owns it.
pub async fn get_principal_session(
    pool: &Pool,
    principal_id: &str,
    session_id: &str,
) -> Result<Option<RunSession>, DbError> {
    let row = sqlx::query(&format!(
        "SELECT {RUN_SESSION_COLUMNS} FROM chat_sessions WHERE id = ? AND principal_id = ?"
    ))
    .bind(session_id)
    .bind(principal_id)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(map_run_session).transpose()
}

/// The principal-owned conversation `turn_id` belongs to; `None` when the
/// turn does not exist or is in a person's chat.
pub async fn run_session_of_turn(
    pool: &Pool,
    turn_id: &str,
) -> Result<Option<RunSession>, DbError> {
    let row = sqlx::query(&format!(
        "SELECT {RUN_SESSION_COLUMNS} FROM chat_sessions \
         WHERE principal_id IS NOT NULL \
           AND id = (SELECT session_id FROM chat_turns WHERE id = ?)"
    ))
    .bind(turn_id)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(map_run_session).transpose()
}

const RUN_SESSION_COLUMNS: &str = "id, principal_id, parent_turn_id, agent_version, visitor_id, \
                                   lang, title, created_at, updated_at";

fn map_run_session(r: &SqliteRow) -> Result<RunSession, DbError> {
    Ok(RunSession {
        id: r.try_get("id")?,
        principal_id: r.try_get("principal_id")?,
        parent_turn_id: r.try_get("parent_turn_id")?,
        agent_version: r.try_get("agent_version")?,
        visitor_id: r.try_get("visitor_id")?,
        lang: r
            .try_get::<Option<String>, _>("lang")?
            .as_deref()
            .and_then(Lang::from_code),
        title: r.try_get("title")?,
        created_at: parse_ts(r.try_get("created_at")?, "created_at")?,
        updated_at: parse_ts(r.try_get("updated_at")?, "updated_at")?,
    })
}

/// Record the language a turn of the principal-owned conversation
/// `session_id` was asked in, so a later resume of it speaks the same one.
pub async fn set_run_lang(pool: &Pool, session_id: &str, lang: Lang) -> Result<(), DbError> {
    sqlx::query("UPDATE chat_sessions SET lang = ? WHERE id = ? AND principal_id IS NOT NULL")
        .bind(lang.code())
        .bind(session_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Who owns `session_id`; `None` when it does not exist.
pub async fn session_owner(pool: &Pool, session_id: &str) -> Result<Option<SessionOwner>, DbError> {
    let row = sqlx::query(r#"SELECT user_id, principal_id FROM chat_sessions WHERE id = ?"#)
        .bind(session_id)
        .fetch_optional(pool)
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let user: Option<String> = row.try_get("user_id")?;
    let principal: Option<String> = row.try_get("principal_id")?;
    match (user, principal) {
        (Some(user), None) => Ok(Some(SessionOwner::User(user))),
        (None, Some(principal)) => Ok(Some(SessionOwner::Principal(principal))),
        _ => Err(DbError::Decode {
            column: "user_id",
            source: anyhow::anyhow!(
                "session `{session_id}` must have exactly one owner (a user or a principal)"
            ),
        }),
    }
}

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
/// principal-owned run, which belongs to no person.
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

    fn run_session(parent_turn_id: Option<&str>) -> NewRunSession<'_> {
        NewRunSession {
            principal_id: "p1",
            title: Some("visitor asks about invoices"),
            parent_turn_id,
            agent_version: Some(3),
        }
    }

    async fn completed_turn(pool: &Pool, session_id: &str, text: &str) -> String {
        let id = Uuid::new_v4().to_string();
        create_user_turn(pool, session_id, &id, text).await.unwrap();
        id
    }

    #[tokio::test]
    async fn a_principal_owned_conversation_is_in_no_persons_view() {
        let pool = pool().await;
        let mine = create_session(&pool, "u1").await.unwrap();
        set_session_title(&pool, &mine.id, "invoices for march")
            .await
            .unwrap();
        let run = create_principal_session(&pool, &run_session(None))
            .await
            .unwrap();
        let run_turn = completed_turn(&pool, &run.id, "invoices please").await;

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
        assert!(get_session(&pool, "u1", &run.id).await.unwrap().is_none());
        assert!(
            get_session_readable(&pool, "u1", &run.id)
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
        assert_eq!(user_for_turn(&pool, &run_turn).await.unwrap(), None);
        assert!(!turn_session_readable(&pool, &run_turn, "u1").await.unwrap());
        assert!(!set_shared(&pool, "u1", &run.id, true).await.unwrap());
        assert!(!delete_session(&pool, "u1", &run.id).await.unwrap());
    }

    #[tokio::test]
    async fn a_run_session_keeps_its_owner_and_link() {
        let pool = pool().await;
        let parent = create_principal_session(&pool, &run_session(None))
            .await
            .unwrap();
        let parent_turn = completed_turn(&pool, &parent.id, "route this").await;
        let child = create_principal_session(&pool, &run_session(Some(&parent_turn)))
            .await
            .unwrap();

        let read = get_principal_session(&pool, "p1", &child.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read, child);
        assert_eq!(read.principal_id, "p1");
        assert_eq!(read.parent_turn_id.as_deref(), Some(parent_turn.as_str()));
        assert_eq!(read.agent_version, Some(3));
        assert_eq!(read.title.as_deref(), Some("visitor asks about invoices"));
        assert!(
            get_principal_session(&pool, "p2", &child.id)
                .await
                .unwrap()
                .is_none(),
            "another principal cannot open the run"
        );
    }

    #[tokio::test]
    async fn a_run_session_keeps_the_language_it_was_last_asked_in() {
        let pool = pool().await;
        let run = create_principal_session(&pool, &run_session(None))
            .await
            .unwrap();
        assert_eq!(run.lang, None);
        set_run_lang(&pool, &run.id, Lang::De).await.unwrap();
        let turn = completed_turn(&pool, &run.id, "Hallo").await;
        let read = run_session_of_turn(&pool, &turn).await.unwrap().unwrap();
        assert_eq!(read.lang, Some(Lang::De));
        set_run_lang(&pool, &run.id, Lang::Fr).await.unwrap();
        let read = get_principal_session(&pool, "p1", &run.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read.lang, Some(Lang::Fr));
    }

    #[tokio::test]
    async fn a_run_is_found_from_any_of_its_turns_and_a_chat_never_is() {
        let pool = pool().await;
        let run = create_principal_session(&pool, &run_session(None))
            .await
            .unwrap();
        let run_turn = completed_turn(&pool, &run.id, "route this").await;
        let mine = create_session(&pool, "u1").await.unwrap();
        let chat_turn = completed_turn(&pool, &mine.id, "hello").await;

        assert_eq!(
            run_session_of_turn(&pool, &run_turn).await.unwrap(),
            Some(run)
        );
        assert_eq!(run_session_of_turn(&pool, &chat_turn).await.unwrap(), None);
        assert_eq!(run_session_of_turn(&pool, "nope").await.unwrap(), None);
    }

    #[tokio::test]
    async fn the_owner_of_a_session_is_a_person_or_a_principal() {
        let pool = pool().await;
        let mine = create_session(&pool, "u1").await.unwrap();
        let run = create_principal_session(&pool, &run_session(None))
            .await
            .unwrap();

        assert_eq!(
            session_owner(&pool, &mine.id).await.unwrap(),
            Some(SessionOwner::User("u1".into()))
        );
        assert_eq!(
            session_owner(&pool, &run.id).await.unwrap(),
            Some(SessionOwner::Principal("p1".into()))
        );
        assert_eq!(session_owner(&pool, "nope").await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_run_session_for_an_unknown_principal_is_refused() {
        let pool = pool().await;
        let refused = create_principal_session(
            &pool,
            &NewRunSession {
                principal_id: "ghost",
                ..run_session(None)
            },
        )
        .await;
        assert!(refused.is_err());
    }
}
