// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The task methods: `SendMessage` (start a task, or answer one waiting for
//! input), `GetTask` and `CancelTask`, and the task as its caller sees it.

use std::time::Duration;

use aiplane_agents::db::run_sessions;
use jiff::Timestamp;
use rama::http::Response;
use serde_json::{Value, json};
use session_core::db::{self as chat, TurnRole, TurnStatus};
use session_core::i18n::{self, Lang, t, t_args};

use super::Call;
use super::envelope::{RpcError, result_response};
use super::params::{SendParams, history_length, parse_send, task_id_param};
use super::stream::stream;
use aiplane_agents::db::a2a_contexts::{self, A2aContext, NewContext};
use aiplane_agents::db::agent_audit::{AuditKind, Correlation, NewEvent};
use aiplane_agents::rates::Inbound;
use aiplane_runtime::agents::a2a::{self as a2a_rt, TaskState};
use aiplane_runtime::agents::embed::{
    self as embed_rt, Admission, Admitted, OpenedTurn, Refusal, TurnWork,
};
use aiplane_runtime::agents::resume::{AgentResume, AgentResumeError, ResumedBy, claim};
use aiplane_runtime::agents::spec::AgentSpec;
use aiplane_runtime::rama_server::state::RamaState;
use aiplane_runtime::suspend::ResumeRefused;

/// How long `CancelTask` waits for a running turn to notice.
const CANCEL_WAIT: Duration = Duration::from_secs(15);

async fn admit(call: &Call, context: Option<&str>) -> Result<Admitted, RpcError> {
    let who = Admission {
        visitor_id: None,
        a2a_context: context,
        ip: call.ip.as_deref(),
    };
    let agent = &call.served.agent.principal.id;
    match embed_rt::admit(&call.state, agent, who, Timestamp::now()).await {
        Ok(admitted) => Ok(admitted),
        Err(refusal) => {
            let retry = refusal.retry_after_secs();
            let mut e = match refusal {
                Refusal::Rate(_) => RpcError::aiplane(
                    "RATE_LIMITED",
                    t_args(
                        call.lang,
                        "agent-embed-rate-limited",
                        &i18n::args([("seconds", retry.into())]),
                    ),
                ),
                Refusal::Budget(_) => {
                    RpcError::aiplane("AGENT_UNAVAILABLE", t(call.lang, "agent-embed-unavailable"))
                }
            }
            .with("retryAfterSeconds", retry.to_string());
            e.retry_after = Some(retry);
            Err(e)
        }
    }
}

async fn audit(call: &Call, action: &str, context: &A2aContext, task: &str) {
    let detail = json!({
        "action": action,
        "context_id": context.session_id,
        "task_id": task,
        "caller_id": call.caller.id,
        "caller_name": call.caller.name,
        "token_id": call.caller.token_id,
    });
    let _ = aiplane_runtime::agents::audit::record_event(
        &call.state.db,
        NewEvent::new(AuditKind::A2aTask, &call.served.agent.principal.id, detail).at(
            Correlation {
                turn_id: Some(task.to_string()),
                session_id: Some(context.session_id.clone()),
                ..Correlation::default()
            },
        ),
    )
    .await;
}

/// Task `task_id` of this agent, if this caller's context holds it: its
/// context and its assistant turn.
async fn find_task(call: &Call, task_id: &str) -> Result<(A2aContext, chat::Turn), RpcError> {
    let db = &call.state.db;
    let agent = &call.served.agent.principal.id;
    let Some(run) = run_sessions::run_session_of_turn(db, task_id)
        .await
        .map_err(RpcError::internal)?
        .filter(|s| &s.principal_id == agent && s.parent_turn_id.is_none())
    else {
        return Err(RpcError::task_not_found(task_id));
    };
    let Some(context) = a2a_contexts::get_for_caller(db, agent, &call.caller.id, &run.id)
        .await
        .map_err(RpcError::internal)?
    else {
        return Err(RpcError::task_not_found(task_id));
    };
    match chat::get_turn(db, &run.id, task_id)
        .await
        .map_err(RpcError::internal)?
    {
        Some(turn) if turn.role == TurnRole::Assistant => Ok((context, turn)),
        _ => Err(RpcError::task_not_found(task_id)),
    }
}

fn timestamp(t: Timestamp) -> String {
    t.strftime("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

fn text_message(id: &str, context: &str, task: &str, role: &str, text: &str) -> Value {
    json!({
        "messageId": id,
        "contextId": context,
        "taskId": task,
        "role": role,
        "parts": [{ "text": text }],
    })
}

/// A task as its caller sees it: the exchange's text and its state, nothing
/// of how it was produced — no tool calls, reasoning, model or upstream
/// error, like a visitor's view of their conversation.
pub(super) struct TaskView {
    pub(super) task: Value,
    pub(super) state: TaskState,
}

pub(super) async fn task_view(
    state: &RamaState,
    principal_id: &str,
    lang: Lang,
    session_id: &str,
    task_id: &str,
    history: Option<usize>,
) -> Result<TaskView, RpcError> {
    let db = &state.db;
    let Some(turn) = chat::get_turn(db, session_id, task_id)
        .await
        .map_err(RpcError::internal)?
    else {
        return Err(RpcError::task_not_found(task_id));
    };
    let suspension = match turn.status {
        TurnStatus::Suspended => chat::get_suspension(db, task_id)
            .await
            .map_err(RpcError::internal)?
            .map(|s| s.view()),
        _ => None,
    };
    let asked = chat::turn_before(db, session_id, turn.seq)
        .await
        .map_err(RpcError::internal)?
        .filter(|t| t.role == TurnRole::User);
    let held = state.chats.holds(principal_id, session_id, task_id);
    let state = TaskState::of(turn.status, held);

    let answer = (state == TaskState::Completed)
        .then(|| turn.content.clone())
        .flatten()
        .filter(|c| !c.is_empty());
    let mut status = json!({
        "state": state.as_str(),
        "timestamp": timestamp(turn.completed_at.unwrap_or(turn.created_at)),
    });
    let mut metadata = None;
    match state {
        TaskState::InputRequired => {
            if let Some(waiting) = suspension.as_ref().map(|s| s.for_participant()) {
                let by_caller = a2a_rt::answered_by_caller(waiting.kind);
                let text = match (&waiting.message, by_caller) {
                    (_, false) => t(lang, "agent-embed-decision-for-staff"),
                    (Some(m), true) => m.clone(),
                    (None, true) => t(lang, "agent-verifier-code-again"),
                };
                status["message"] = text_message(task_id, session_id, task_id, "ROLE_AGENT", &text);
                metadata = Some(json!({ "aiplane": {
                    "kind": waiting.kind.as_str(),
                    "answeredBy": if by_caller { "caller" } else { "staff" },
                    "requestId": waiting.request_id,
                    "expiresAt": timestamp(waiting.expires_at),
                } }));
            }
        }
        TaskState::Failed => {
            status["message"] = text_message(
                task_id,
                session_id,
                task_id,
                "ROLE_AGENT",
                &t(lang, "embed-error-generic"),
            );
        }
        _ => {}
    }

    let mut messages = Vec::new();
    if let Some(user) = &asked
        && let Some(text) = user.user_content.as_deref()
    {
        messages.push(text_message(
            &user.id,
            session_id,
            task_id,
            "ROLE_USER",
            text,
        ));
    }
    if let Some(text) = &answer {
        messages.push(text_message(
            task_id,
            session_id,
            task_id,
            "ROLE_AGENT",
            text,
        ));
    }
    let mut task = json!({
        "id": task_id,
        "contextId": session_id,
        "status": status,
    });
    if let Some(text) = &answer {
        task["artifacts"] = json!([answer_artifact(text)]);
    }
    match history {
        Some(0) => {}
        Some(n) => {
            let skip = messages.len().saturating_sub(n);
            task["history"] = Value::Array(messages.split_off(skip));
        }
        None => task["history"] = Value::Array(messages),
    }
    if let Some(m) = metadata {
        task["metadata"] = m;
    }
    Ok(TaskView { task, state })
}

fn answer_artifact(text: &str) -> Value {
    json!({ "artifactId": "answer", "name": "answer", "parts": [{ "text": text }] })
}

/// A started (or resumed) task's run, spawned so it outlives the request:
/// the turn finishes even if the caller hangs up.
type Running = tokio::task::JoinHandle<()>;

/// Respond to a send once its run is under way: blocking (the default) waits
/// for the task to end or pause, `returnImmediately` answers at once, a
/// streaming call streams.
async fn respond(
    call: &Call,
    session_id: &str,
    task_id: &str,
    run: Running,
    p: &SendParams,
    streaming: bool,
) -> Result<Response, RpcError> {
    if streaming {
        return stream(call, session_id, task_id).await;
    }
    if !p.return_immediately
        && let Err(err) = run.await
    {
        tracing::warn!(error = %err, task = task_id, "an A2A task's run panicked");
    }
    let view = task_view(
        &call.state,
        &call.served.agent.principal.id,
        call.lang,
        session_id,
        task_id,
        p.history_length,
    )
    .await?;
    Ok(result_response(&call.id, json!({ "task": view.task })))
}

pub(super) async fn send_message(
    call: &Call,
    params: &Value,
    streaming: bool,
) -> Result<Response, RpcError> {
    let p = parse_send(params)?;
    match p.task_id.clone() {
        Some(task) => continue_task(call, &p, &task, streaming).await,
        None => start_task(call, &p, streaming).await,
    }
}

/// The caller's context the message names, if it names one.
async fn resolve_context(
    call: &Call,
    context_id: Option<&str>,
) -> Result<Option<A2aContext>, RpcError> {
    let Some(id) = context_id else {
        return Ok(None);
    };
    a2a_contexts::get_for_caller(
        &call.state.db,
        &call.served.agent.principal.id,
        &call.caller.id,
        id,
    )
    .await
    .map_err(RpcError::internal)?
    .map(Some)
    .ok_or_else(|| {
        RpcError::invalid_params(format!(
            "`message.contextId` `{id}` is not a context of yours on this agent — leave it out \
             to start a new one"
        ))
    })
}

async fn open_context(call: &Call) -> Result<A2aContext, RpcError> {
    a2a_contexts::open(
        &call.state.db,
        &NewContext {
            agent_id: &call.served.agent.principal.id,
            agent_version: call.served.live_version,
            caller_id: &call.caller.id,
            caller_name: &call.caller.name,
            token_id: &call.caller.token_id,
            client_ip: call.ip.as_deref(),
            now: Timestamp::now(),
        },
    )
    .await
    .map_err(RpcError::internal)
}

/// Refuse a new task while the context still runs one or waits on one.
async fn ensure_idle(call: &Call, session_id: &str) -> Result<(), RpcError> {
    let db = &call.state.db;
    if let Some(busy) = chat::in_flight_turn(db, session_id)
        .await
        .map_err(RpcError::internal)?
    {
        return Err(RpcError::task_in_progress(&busy.id));
    }
    if let Some(waiting) = chat::suspended_turn_in_session(db, session_id)
        .await
        .map_err(RpcError::internal)?
    {
        return Err(RpcError::aiplane(
            "CONTEXT_WAITING",
            format!(
                "this context waits on task `{waiting}` — answer it (send the message with that \
                 `taskId`), cancel it, or start a new context"
            ),
        )
        .with("taskId", waiting));
    }
    Ok(())
}

/// The version the context is pinned to (the live one for a new context)
/// and the model its main run uses.
async fn turn_model(call: &Call, session_id: &str) -> Result<(i64, String), RpcError> {
    let state = &call.state;
    let agent = &call.served.agent.principal.id;
    let version = run_sessions::get_principal_session(&state.db, agent, session_id)
        .await
        .map_err(RpcError::internal)?
        .and_then(|run| run.agent_version)
        .unwrap_or(call.served.live_version);
    let compiled = state
        .agent_specs
        .version(&state.db, agent, version)
        .await
        .map_err(RpcError::internal)?;
    let spec = compiled
        .as_deref()
        .and_then(|c| c.agent().ok())
        .unwrap_or(AgentSpec::empty());
    let model = aiplane_runtime::agents::defaults::main_model(state, spec)
        .await
        .unwrap_or_default();
    Ok((version, model))
}

async fn start_task(call: &Call, p: &SendParams, streaming: bool) -> Result<Response, RpcError> {
    let state = &call.state;
    let existing = resolve_context(call, p.context_id.as_deref()).await?;
    let admitted = admit(call, existing.as_ref().map(|c| c.session_id.as_str())).await?;
    let Some(runner) = state.agent_runner.clone() else {
        return Err(RpcError::runtime_unavailable());
    };
    let context = match existing {
        Some(c) => c,
        None => {
            let opened = open_context(call).await?;
            admitted
                .opened(state, Inbound::A2a(&opened.session_id))
                .await;
            opened
        }
    };
    let session_id = context.session_id.clone();
    let user_turn = uuid::Uuid::new_v4().to_string();
    let task_id = uuid::Uuid::new_v4().to_string();
    let agent = &call.served.agent.principal.id;
    let Some(hold) = embed_rt::claim(&state.chats, agent, &session_id, &task_id) else {
        return Err(RpcError::task_in_progress(""));
    };
    ensure_idle(call, &session_id).await?;
    let (version, model) = turn_model(call, &session_id).await?;

    chat::create_user_turn(&state.db, &session_id, &user_turn, &p.text)
        .await
        .map_err(RpcError::internal)?;
    chat::create_assistant_turn_in_progress(&state.db, &session_id, &task_id, &model)
        .await
        .map_err(RpcError::internal)?;
    audit(call, "message", &context, &task_id).await;

    let turn = OpenedTurn {
        agent_id: call.served.agent.principal.id.clone(),
        version,
        session_id: session_id.clone(),
        turn_id: task_id.clone(),
        visitor_id: None,
        caller: Some(context.caller()),
        lang: Some(call.lang),
    };
    let run = embed_rt::spawn_guarded(state.clone(), runner, hold, TurnWork::Run(turn));
    respond(call, &session_id, &task_id, run, p, streaming).await
}

/// A message naming a task: the answer to what an `input-required` task
/// waits for, when the caller may give it.
async fn continue_task(
    call: &Call,
    p: &SendParams,
    task_id: &str,
    streaming: bool,
) -> Result<Response, RpcError> {
    let state = &call.state;
    let (context, turn) = find_task(call, task_id).await?;
    if let Some(asked) = &p.context_id
        && asked != &context.session_id
    {
        return Err(RpcError::invalid_params(format!(
            "task `{task_id}` belongs to context `{}`, not `{asked}` — send the task's own \
             contextId or leave it out",
            context.session_id
        )));
    }
    let session_id = context.session_id.clone();
    let agent = &call.served.agent.principal.id;
    match TaskState::of(turn.status, state.chats.holds(agent, &session_id, task_id)) {
        TaskState::Working => return Err(RpcError::task_in_progress(task_id)),
        s if s.is_terminal() => {
            return Err(RpcError::unsupported(format!(
                "task `{task_id}` is {} and takes no further message — send the message with \
                 only `contextId` to start a new task in the same context",
                s.as_str()
            )));
        }
        _ => {}
    }
    admit(call, Some(&session_id)).await?;
    let Some(runner) = state.agent_runner.clone() else {
        return Err(RpcError::runtime_unavailable());
    };
    let Some(hold) = embed_rt::claim(&state.chats, agent, &session_id, task_id) else {
        return Err(RpcError::task_in_progress(task_id));
    };
    let decision = crate::pages::chat::json_api::decision_from(
        chat::DecisionKind::Value,
        Some(Value::String(p.text.clone())),
    )
    .map_err(RpcError::invalid_params)?;
    let claimed = claim(
        state,
        AgentResume {
            agent_id: &call.served.agent.principal.id,
            session_id: &session_id,
            turn_id: task_id,
            request_id: None,
            decision,
            by: ResumedBy::Participant,
        },
    )
    .await
    .map_err(|err| resume_refused(err, call.lang, task_id))?;
    audit(call, "input", &context, task_id).await;
    let run = embed_rt::spawn_guarded(state.clone(), runner, hold, TurnWork::Resume(claimed));
    respond(call, &session_id, task_id, run, p, streaming).await
}

fn resume_refused(err: AgentResumeError, lang: Lang, task: &str) -> RpcError {
    match err {
        AgentResumeError::StaffOnly { kind } => RpcError::aiplane(
            "DECISION_FOR_STAFF",
            t(lang, "agent-embed-decision-for-staff"),
        )
        .with("kind", kind)
        .with("taskId", task),
        AgentResumeError::Refused(ResumeRefused::NotSuspended)
        | AgentResumeError::Refused(ResumeRefused::StaleRequest { .. }) => {
            RpcError::aiplane("NOT_WAITING", t(lang, "agent-embed-not-waiting"))
                .with("taskId", task)
        }
        AgentResumeError::Db(err) => RpcError::internal(err),
        other => RpcError::invalid_params(other.to_string()),
    }
}

pub(super) async fn get_task(call: &Call, params: &Value) -> Result<Response, RpcError> {
    let id = task_id_param(params)?;
    let history = history_length(params.get("historyLength"), "params.historyLength")?;
    let (context, _) = find_task(call, &id).await?;
    let view = task_view(
        &call.state,
        &call.served.agent.principal.id,
        call.lang,
        &context.session_id,
        &id,
        history,
    )
    .await?;
    Ok(result_response(&call.id, view.task))
}

fn not_cancelable(task: &str, state: TaskState) -> RpcError {
    RpcError::new(
        -32002,
        "TASK_NOT_CANCELABLE",
        format!(
            "task `{task}` is {} and can no longer be cancelled",
            state.as_str()
        ),
    )
    .with("taskId", task)
}

pub(super) async fn cancel_task(call: &Call, params: &Value) -> Result<Response, RpcError> {
    let id = task_id_param(params)?;
    let (context, turn) = find_task(call, &id).await?;
    let session_id = &context.session_id;
    let agent = &call.served.agent.principal.id;
    let workers = &call.state.chats;
    match TaskState::of(turn.status, workers.holds(agent, session_id, &id)) {
        s if s.is_terminal() => return Err(not_cancelable(&id, s)),
        TaskState::Working => {
            if !workers.cancel_turn(agent, session_id, &id) {
                return Err(not_cancelable(&id, TaskState::Working));
            }
            let _ = tokio::time::timeout(
                CANCEL_WAIT,
                embed_rt::released(workers, agent, session_id, &id),
            )
            .await;
        }
        _ => {
            let Some(_hold) = embed_rt::claim(workers, agent, session_id, &id) else {
                return Err(RpcError::task_in_progress(&id));
            };
            cancel_paused(call, &id).await?;
        }
    }
    audit(call, "cancel", &context, &id).await;
    let view = task_view(
        &call.state,
        &call.served.agent.principal.id,
        call.lang,
        session_id,
        &id,
        None,
    )
    .await?;
    Ok(result_response(&call.id, view.task))
}

/// End a paused task: the conversation's turn and every sub-agent turn it
/// waits on, outermost first, each `cancelled` with its waiting call.
async fn cancel_paused(call: &Call, task: &str) -> Result<(), RpcError> {
    let db = &call.state.db;
    let mut next = Some(task.to_string());
    let mut depth = 0;
    while let Some(turn) = next.take() {
        let child = chat::get_suspension(db, &turn)
            .await
            .map_err(RpcError::internal)?
            .and_then(|s| s.child_turn);
        chat::cancel_suspended_turn(db, &turn)
            .await
            .map_err(RpcError::internal)?;
        depth += 1;
        if depth < aiplane_core::server::run_chain::MAX_DEPTH {
            next = child;
        }
    }
    Ok(())
}

pub(super) async fn subscribe(call: &Call, params: &Value) -> Result<Response, RpcError> {
    let id = task_id_param(params)?;
    let (context, turn) = find_task(call, &id).await?;
    let state = TaskState::of(
        turn.status,
        call.state
            .chats
            .holds(&call.served.agent.principal.id, &context.session_id, &id),
    );
    if state.is_terminal() {
        return Err(RpcError::unsupported(format!(
            "task `{id}` is {} — read it with GetTask",
            state.as_str()
        )));
    }
    stream(call, &context.session_id, &id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_utc_with_milliseconds() {
        let t: Timestamp = "2026-10-02T08:09:10.123456Z".parse().unwrap();
        assert_eq!(timestamp(t), "2026-10-02T08:09:10.123Z");
    }
}
