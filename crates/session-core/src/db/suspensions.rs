// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Turns paused at a tool call that waits for a decision from outside the
//! model (`chat_turn_suspensions`, migration 0078).
//!
//! A suspension is the durable half of what `FeedbackHub` does in memory: the
//! run state at the pause point is written down, the turn's status becomes
//! [`TurnStatus::Suspended`], and the worker is freed. Resuming claims the row
//! (deleting it and flipping the turn back to `in_progress` in one
//! transaction) and hands it to the driver, which continues the same turn.

use super::*;

/// What a suspended call is waiting for. Decides which decisions are offered.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuspensionKind {
    /// Run this call, or not.
    Approval,
    /// A value only the user may type, which never passes through the model.
    SecureInput,
    /// An answer from a human other than the one chatting.
    HumanAnswer,
}

impl SuspensionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approval => "approval",
            Self::SecureInput => "secure_input",
            Self::HumanAnswer => "human_answer",
        }
    }

    fn parse(s: &str) -> Result<Self, DbError> {
        match s {
            "approval" => Ok(Self::Approval),
            "secure_input" => Ok(Self::SecureInput),
            "human_answer" => Ok(Self::HumanAnswer),
            _ => Err(DbError::Decode {
                column: "kind",
                source: anyhow::anyhow!("unknown suspension kind `{s}`"),
            }),
        }
    }

    /// The decisions a resume may carry for this kind. Deny is always one of
    /// them: whoever is asked can always refuse.
    pub fn options(self) -> &'static [DecisionKind] {
        match self {
            Self::Approval => &[DecisionKind::AllowOnce, DecisionKind::Deny],
            Self::SecureInput | Self::HumanAnswer => &[DecisionKind::Value, DecisionKind::Deny],
        }
    }
}

/// The shape of a decision, without its payload — what a client is offered.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    AllowOnce,
    Deny,
    Value,
}

/// Why a call was denied. The model is told which, because "the user said no"
/// and "nobody answered in time" call for different next steps.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    User,
    Timeout,
}

/// What a resume carries into the waiting call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    AllowOnce,
    Deny { reason: DenyReason },
    Value { value: serde_json::Value },
}

impl Decision {
    pub fn kind(&self) -> DecisionKind {
        match self {
            Self::AllowOnce => DecisionKind::AllowOnce,
            Self::Deny { .. } => DecisionKind::Deny,
            Self::Value { .. } => DecisionKind::Value,
        }
    }
}

/// What an expired suspension resolves to. Deny unless the requesting tool
/// said otherwise: silence is not consent.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeoutFallback {
    #[default]
    Deny,
    AllowOnce,
}

impl TimeoutFallback {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::AllowOnce => "allow_once",
        }
    }

    fn parse(s: &str) -> Result<Self, DbError> {
        match s {
            "deny" => Ok(Self::Deny),
            "allow_once" => Ok(Self::AllowOnce),
            _ => Err(DbError::Decode {
                column: "on_timeout",
                source: anyhow::anyhow!("unknown timeout fallback `{s}`"),
            }),
        }
    }

    /// The decision a timeout stands in for.
    pub fn decision(self) -> Decision {
        match self {
            Self::Deny => Decision::Deny {
                reason: DenyReason::Timeout,
            },
            Self::AllowOnce => Decision::AllowOnce,
        }
    }
}

/// The tool call a suspended turn is waiting on, as the model issued it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCall {
    pub id: String,
    pub name: String,
    /// The model's raw JSON arguments string.
    pub arguments: String,
}

/// What the turn had spent when it paused, so a resumed run continues against
/// the same budget instead of starting a fresh one.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetUsed {
    pub rounds: u32,
    pub seconds: u64,
    pub tokens: u64,
}

/// One paused turn: everything a resume needs to continue it.
#[derive(Clone, Debug, PartialEq)]
pub struct TurnSuspension {
    pub turn_id: String,
    /// What a client answers. A fresh one per pause, so an answer meant for
    /// an earlier pause of the same turn cannot settle a later one.
    pub request_id: String,
    pub kind: SuspensionKind,
    /// Shown next to the decision, when the tool has something to say.
    pub message: Option<String>,
    pub tool_call: PendingCall,
    /// This turn's round messages so far, without the waiting call's result.
    /// The history and system message before them are rebuilt on resume.
    pub tail: Vec<serde_json::Value>,
    pub budget_used: BudgetUsed,
    /// Set when the pause is inside a sub-agent run: the child turn that
    /// actually waits. Resuming continues there first, then here.
    pub child_turn: Option<String>,
    pub on_timeout: TimeoutFallback,
    pub expires_at: Timestamp,
    pub created_at: Timestamp,
}

impl TurnSuspension {
    /// The part a client may see: enough to render the decision, none of the
    /// run state.
    pub fn view(&self) -> SuspensionView {
        SuspensionView {
            request_id: self.request_id.clone(),
            kind: self.kind,
            message: self.message.clone(),
            tool_call_id: self.tool_call.id.clone(),
            tool: self.tool_call.name.clone(),
            options: self.kind.options().to_vec(),
            expires_at: self.expires_at,
        }
    }
}

/// A suspension as the client sees it, on the turn and in the `suspended`
/// event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuspensionView {
    pub request_id: String,
    pub kind: SuspensionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub tool_call_id: String,
    pub tool: String,
    pub options: Vec<DecisionKind>,
    pub expires_at: Timestamp,
}

/// A suspension past its deadline, with the owner a resume runs as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpiredSuspension {
    pub turn_id: String,
    pub session_id: String,
    pub user_id: String,
    pub on_timeout: TimeoutFallback,
}

const SELECT_SUSPENSION: &str = r#"SELECT turn_id, request_id, kind, message, tool_call, tail,
                                           budget_used, child_turn, on_timeout, expires_at,
                                           created_at
                                    FROM chat_turn_suspensions"#;

fn json_column<T: serde::de::DeserializeOwned>(
    row: &SqliteRow,
    column: &'static str,
) -> Result<T, DbError> {
    let raw: String = row.try_get(column)?;
    serde_json::from_str(&raw).map_err(|e| DbError::Decode {
        column,
        source: e.into(),
    })
}

fn map_suspension(row: &SqliteRow) -> Result<TurnSuspension, DbError> {
    let kind: String = row.try_get("kind")?;
    let on_timeout: String = row.try_get("on_timeout")?;
    Ok(TurnSuspension {
        turn_id: row.try_get("turn_id")?,
        request_id: row.try_get("request_id")?,
        kind: SuspensionKind::parse(&kind)?,
        message: row.try_get("message")?,
        tool_call: json_column(row, "tool_call")?,
        tail: json_column(row, "tail")?,
        budget_used: json_column(row, "budget_used")?,
        child_turn: row.try_get("child_turn")?,
        on_timeout: TimeoutFallback::parse(&on_timeout)?,
        expires_at: parse_ts(row.try_get("expires_at")?, "expires_at")?,
        created_at: parse_ts(row.try_get("created_at")?, "created_at")?,
    })
}

fn to_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("plain data serializes")
}

/// Pause a running turn. Writes the row and flips the turn to `suspended` in
/// one transaction. Returns `false`, and writes nothing, when the turn is not
/// `in_progress` any more (cancelled or swept underneath the driver).
pub async fn suspend_turn(pool: &Pool, suspension: &TurnSuspension) -> Result<bool, DbError> {
    let mut tx = pool.begin().await?;
    let flipped = sqlx::query(
        r#"UPDATE chat_turns SET status = 'suspended'
           WHERE id = ? AND role = 'assistant' AND status = 'in_progress'"#,
    )
    .bind(&suspension.turn_id)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        > 0;
    if !flipped {
        return Ok(false);
    }
    sqlx::query(
        r#"INSERT INTO chat_turn_suspensions
              (turn_id, request_id, kind, message, tool_call, tail, budget_used,
               child_turn, on_timeout, expires_at, created_at)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
    )
    .bind(&suspension.turn_id)
    .bind(&suspension.request_id)
    .bind(suspension.kind.as_str())
    .bind(&suspension.message)
    .bind(to_json(&suspension.tool_call))
    .bind(to_json(&suspension.tail))
    .bind(to_json(&suspension.budget_used))
    .bind(&suspension.child_turn)
    .bind(suspension.on_timeout.as_str())
    .bind(suspension.expires_at.to_string())
    .bind(suspension.created_at.to_string())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// The suspension a turn is paused on, if any.
pub async fn get_suspension(pool: &Pool, turn_id: &str) -> Result<Option<TurnSuspension>, DbError> {
    let row = sqlx::query(&format!("{SELECT_SUSPENSION} WHERE turn_id = ?"))
        .bind(turn_id)
        .fetch_optional(pool)
        .await?;
    row.as_ref().map(map_suspension).transpose()
}

/// Take a turn's suspension for resuming: delete the row and flip the turn
/// back to `in_progress`, in one transaction. `None` when there is nothing to
/// take — never suspended, or another resume got there first.
pub async fn claim_suspension(
    pool: &Pool,
    turn_id: &str,
) -> Result<Option<TurnSuspension>, DbError> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "DELETE FROM chat_turn_suspensions WHERE turn_id = ? \
         RETURNING turn_id, request_id, kind, message, tool_call, tail, budget_used, \
                   child_turn, on_timeout, expires_at, created_at",
    )
    .bind(turn_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let suspension = map_suspension(&row)?;
    sqlx::query(
        r#"UPDATE chat_turns SET status = 'in_progress' WHERE id = ? AND status = 'suspended'"#,
    )
    .bind(turn_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(suspension))
}

/// Give up on a paused turn: it ends `cancelled`, like a stopped one, and its
/// waiting call is recorded as never run. Returns whether anything was paused.
pub async fn cancel_suspended_turn(pool: &Pool, turn_id: &str) -> Result<bool, DbError> {
    let now = Timestamp::now().to_string();
    let mut tx = pool.begin().await?;
    let removed = sqlx::query("DELETE FROM chat_turn_suspensions WHERE turn_id = ?")
        .bind(turn_id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
        > 0;
    if !removed {
        return Ok(false);
    }
    sqlx::query(
        r#"UPDATE chat_turns SET status = 'cancelled', completed_at = ?
           WHERE id = ? AND status = 'suspended'"#,
    )
    .bind(&now)
    .bind(turn_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        r#"UPDATE chat_tool_calls
           SET status = 'errored',
               output_json = COALESCE(output_json, ?),
               completed_at = ?
           WHERE turn_id = ? AND status = 'running'"#,
    )
    .bind(serde_json::Value::String("Cancelled while waiting for a decision.".into()).to_string())
    .bind(&now)
    .bind(turn_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// The paused turn of a conversation, if it has one. A conversation has at
/// most one: a suspended turn holds it the way a running worker does.
pub async fn suspended_turn_in_session(
    pool: &Pool,
    session_id: &str,
) -> Result<Option<String>, DbError> {
    let row = sqlx::query(
        r#"SELECT s.turn_id FROM chat_turn_suspensions s
           JOIN chat_turns t ON t.id = s.turn_id
           WHERE t.session_id = ?
           LIMIT 1"#,
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await?;
    row.map(|r| r.try_get("turn_id"))
        .transpose()
        .map_err(Into::into)
}

/// Every suspension whose deadline has passed at `now`, oldest deadline first.
///
/// Compared after parsing rather than in SQL: RFC 3339 strings with and
/// without fractional seconds do not sort as the instants they name. The
/// table only holds turns that are waiting, so reading it whole is cheap.
///
/// Only person-owned conversations: the chat path resumes as the owner, and a
/// principal-owned run has no person to resume as.
pub async fn expired_suspensions(
    pool: &Pool,
    now: Timestamp,
) -> Result<Vec<ExpiredSuspension>, DbError> {
    let rows = sqlx::query(
        r#"SELECT s.turn_id, s.on_timeout, s.expires_at, t.session_id, cs.user_id
           FROM chat_turn_suspensions s
           JOIN chat_turns t ON t.id = s.turn_id
           JOIN chat_sessions cs ON cs.id = t.session_id
           WHERE cs.user_id IS NOT NULL"#,
    )
    .fetch_all(pool)
    .await?;
    let mut expired = Vec::new();
    for row in &rows {
        let expires_at = parse_ts(row.try_get("expires_at")?, "expires_at")?;
        if expires_at > now {
            continue;
        }
        let on_timeout: String = row.try_get("on_timeout")?;
        expired.push((
            expires_at,
            ExpiredSuspension {
                turn_id: row.try_get("turn_id")?,
                session_id: row.try_get("session_id")?,
                user_id: row.try_get("user_id")?,
                on_timeout: TimeoutFallback::parse(&on_timeout)?,
            },
        ));
    }
    expired.sort_by_key(|(at, _)| *at);
    Ok(expired.into_iter().map(|(_, s)| s).collect())
}

/// The client views of every suspension in a session, by turn id — the
/// one-query-then-bucket read `list_turns` does for its side tables.
pub(crate) async fn suspension_views_for_session(
    pool: &Pool,
    session_id: &str,
) -> Result<std::collections::HashMap<String, SuspensionView>, DbError> {
    let rows = sqlx::query(&format!(
        "{SELECT_SUSPENSION} WHERE turn_id IN (SELECT id FROM chat_turns WHERE session_id = ?)"
    ))
    .bind(session_id)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| map_suspension(row).map(|s| (s.turn_id.clone(), s.view())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::tests::pool;

    fn suspension(turn_id: &str, expires_at: Timestamp) -> TurnSuspension {
        TurnSuspension {
            turn_id: turn_id.into(),
            request_id: format!("req-{turn_id}"),
            kind: SuspensionKind::Approval,
            message: None,
            tool_call: PendingCall {
                id: "call-1".into(),
                name: "company_echo".into(),
                arguments: r#"{"message":"hi"}"#.into(),
            },
            tail: vec![serde_json::json!({"role": "assistant", "content": null})],
            budget_used: BudgetUsed {
                rounds: 1,
                seconds: 3,
                tokens: 120,
            },
            child_turn: None,
            on_timeout: TimeoutFallback::Deny,
            expires_at,
            created_at: Timestamp::now(),
        }
    }

    fn in_an_hour() -> Timestamp {
        Timestamp::now() + jiff::SignedDuration::from_hours(1)
    }

    async fn running_turn(pool: &Pool, turn_id: &str) -> String {
        let s = create_session(pool, "u1").await.unwrap();
        create_user_turn(pool, &s.id, &format!("{turn_id}-u"), "hi")
            .await
            .unwrap();
        create_assistant_turn_in_progress(pool, &s.id, turn_id, "m")
            .await
            .unwrap();
        insert_running_tool_call(pool, turn_id, "call-1", "company_echo", "{}")
            .await
            .unwrap();
        s.id
    }

    #[tokio::test]
    async fn a_suspended_turn_carries_its_decision_to_every_reader() {
        let pool = pool().await;
        let session_id = running_turn(&pool, "a1").await;
        let row = suspension("a1", in_an_hour());
        assert!(suspend_turn(&pool, &row).await.unwrap());

        let listed = list_turns(&pool, &session_id).await.unwrap();
        let turn = listed.iter().find(|t| t.turn.id == "a1").unwrap();
        assert_eq!(turn.turn.status, TurnStatus::Suspended);
        assert_eq!(turn.suspension, Some(row.view()));
        assert_eq!(
            turn.suspension.as_ref().unwrap().options,
            [DecisionKind::AllowOnce, DecisionKind::Deny]
        );
        let one = get_turn_with_tools(&pool, &session_id, "a1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(one.suspension, Some(row.view()));
        assert_eq!(get_suspension(&pool, "a1").await.unwrap(), Some(row));
    }

    #[tokio::test]
    async fn only_a_running_turn_can_be_suspended() {
        let pool = pool().await;
        running_turn(&pool, "a1").await;
        finalize_turn(&pool, "a1", TurnStatus::Cancelled, None)
            .await
            .unwrap();
        assert!(
            !suspend_turn(&pool, &suspension("a1", in_an_hour()))
                .await
                .unwrap()
        );
        assert_eq!(get_suspension(&pool, "a1").await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_suspension_is_claimed_exactly_once() {
        let pool = pool().await;
        let session_id = running_turn(&pool, "a1").await;
        let row = suspension("a1", in_an_hour());
        suspend_turn(&pool, &row).await.unwrap();

        assert_eq!(claim_suspension(&pool, "a1").await.unwrap(), Some(row));
        assert_eq!(claim_suspension(&pool, "a1").await.unwrap(), None);
        let turn = get_turn(&pool, &session_id, "a1").await.unwrap().unwrap();
        assert_eq!(turn.status, TurnStatus::InProgress);
    }

    /// A restart must not take a paused turn for a crashed one: its waiting
    /// call stays `running` and the turn stays suspended.
    #[tokio::test]
    async fn a_suspended_turn_survives_the_startup_sweep_and_the_orphan_recovery() {
        let pool = pool().await;
        let session_id = running_turn(&pool, "a1").await;
        suspend_turn(&pool, &suspension("a1", in_an_hour()))
            .await
            .unwrap();

        assert_eq!(sweep_in_progress_at_startup(&pool).await.unwrap(), 0);
        mark_orphaned_in_progress_as_errored(&pool, &session_id, None)
            .await
            .unwrap();

        let turn = get_turn_with_tools(&pool, &session_id, "a1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(turn.turn.status, TurnStatus::Suspended);
        assert_eq!(turn.tool_calls[0].status, ToolCallStatus::Running);
    }

    #[tokio::test]
    async fn only_suspensions_past_their_deadline_are_expired() {
        let pool = pool().await;
        running_turn(&pool, "late").await;
        running_turn(&pool, "early").await;
        running_turn(&pool, "waiting").await;
        let now = Timestamp::now();
        let mut late = suspension("late", now - jiff::SignedDuration::from_secs(5));
        late.on_timeout = TimeoutFallback::AllowOnce;
        suspend_turn(&pool, &late).await.unwrap();
        suspend_turn(
            &pool,
            &suspension("early", now - jiff::SignedDuration::from_mins(5)),
        )
        .await
        .unwrap();
        suspend_turn(&pool, &suspension("waiting", in_an_hour()))
            .await
            .unwrap();

        let expired = expired_suspensions(&pool, now).await.unwrap();
        let ids: Vec<_> = expired.iter().map(|e| e.turn_id.as_str()).collect();
        assert_eq!(ids, ["early", "late"]);
        assert_eq!(expired[1].user_id, "u1");
        assert_eq!(expired[1].on_timeout, TimeoutFallback::AllowOnce);
    }

    /// A run owned by a system principal has no person to resume as; it is
    /// settled by the agent path, never by the chat sweeper.
    #[tokio::test]
    async fn the_chat_sweeper_never_sees_a_principal_owned_run() {
        let pool = pool().await;
        let run = create_principal_session(
            &pool,
            &NewRunSession {
                principal_id: "p1",
                title: None,
                parent_turn_id: None,
                agent_version: None,
            },
        )
        .await
        .unwrap();
        create_user_turn(&pool, &run.id, "agent-u", "hi")
            .await
            .unwrap();
        create_assistant_turn_in_progress(&pool, &run.id, "agent-turn", "m")
            .await
            .unwrap();
        insert_running_tool_call(&pool, "agent-turn", "call-1", "company_echo", "{}")
            .await
            .unwrap();
        let now = Timestamp::now();
        suspend_turn(
            &pool,
            &suspension("agent-turn", now - jiff::SignedDuration::from_secs(5)),
        )
        .await
        .unwrap();

        assert!(expired_suspensions(&pool, now).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn cancelling_a_paused_turn_ends_it_and_its_waiting_call() {
        let pool = pool().await;
        let session_id = running_turn(&pool, "a1").await;
        suspend_turn(&pool, &suspension("a1", in_an_hour()))
            .await
            .unwrap();

        assert!(cancel_suspended_turn(&pool, "a1").await.unwrap());
        assert!(!cancel_suspended_turn(&pool, "a1").await.unwrap());
        let turn = get_turn_with_tools(&pool, &session_id, "a1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(turn.turn.status, TurnStatus::Cancelled);
        assert_eq!(turn.suspension, None);
        assert_eq!(turn.tool_calls[0].status, ToolCallStatus::Errored);
        assert_eq!(
            suspended_turn_in_session(&pool, &session_id).await.unwrap(),
            None
        );
    }

    /// A paused turn holds its conversation the way a running worker does, so
    /// a message queued behind it waits for the decision.
    #[tokio::test]
    async fn a_waiting_message_does_not_start_in_a_paused_conversation() {
        let pool = pool().await;
        let session_id = running_turn(&pool, "a1").await;
        suspend_turn(&pool, &suspension("a1", in_an_hour()))
            .await
            .unwrap();
        assert_eq!(
            suspended_turn_in_session(&pool, &session_id).await.unwrap(),
            Some("a1".into())
        );
        create_user_turn(&pool, &session_id, "next", "and then?")
            .await
            .unwrap();
        insert_pending_turn(
            &pool,
            &PendingTurn {
                turn_id: "next".into(),
                session_id: session_id.clone(),
                user_id: "u1".into(),
                model: "m".into(),
                voice: false,
                client_ip: None,
                secure: false,
                created_at: Timestamp::now(),
            },
        )
        .await
        .unwrap();

        assert_eq!(take_next_for_user(&pool, "u1", &[]).await.unwrap(), None);
        claim_suspension(&pool, "a1").await.unwrap();
        assert_eq!(
            take_next_for_user(&pool, "u1", &[])
                .await
                .unwrap()
                .map(|p| p.turn_id),
            Some("next".into())
        );
    }

    #[test]
    fn a_decision_reads_as_the_resume_body_names_it() {
        let deny: Decision =
            serde_json::from_value(serde_json::json!({"decision": "deny", "reason": "user"}))
                .unwrap();
        assert_eq!(deny.kind(), DecisionKind::Deny);
        let value: Decision =
            serde_json::from_value(serde_json::json!({"decision": "value", "value": "s3cret"}))
                .unwrap();
        assert_eq!(
            value,
            Decision::Value {
                value: serde_json::json!("s3cret")
            }
        );
        assert_eq!(
            TimeoutFallback::default().decision(),
            Decision::Deny {
                reason: DenyReason::Timeout
            }
        );
    }
}
