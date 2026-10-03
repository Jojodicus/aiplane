// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! `/a2a/agents/{id}` — an agent served to other agent platforms over A2A
//! (Agent2Agent protocol v1.0, JSON-RPC binding; `docs/agents.md` "What
//! #102 built").
//!
//! - `GET …/agent-card.json` is the agent card, public, and only for an
//!   enabled agent whose live version sets `publish.a2a.enabled: true`.
//! - `POST …` is the JSON-RPC endpoint: `SendMessage`,
//!   `SendStreamingMessage`, `GetTask`, `CancelTask`, `SubscribeToTask`.
//!
//! A caller is a system principal holding the `a2a_caller` grant for this
//! agent, authenticated with its `gws_` token. A context is an agent
//! conversation owned by the agent's principal (`a2a_contexts` records the
//! caller); a task is one assistant turn of it. Every task runs exactly as
//! an embed visitor's message does — the same admission (rates, owner
//! budget), the same runner, the same buffered answer behind the output
//! filter, the same suspend/resume — so nothing here touches the loop. The
//! caller rides in the run's call chain, so every audit and usage row of the
//! task names it.
//!
//! The path is not under `/api/v0`: like `/v1`, it is a protocol surface
//! whose shape the protocol defines, not one of the SPA's JSON routes.

use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use rama::http::service::web::extract::State;
use rama::http::{Body, HeaderMap, HeaderValue, Request, Response, StatusCode, header};
use serde_json::{Map, Value, json};
use session_core::chat_json::{SseTx, json_stream_response};
use session_core::db::{self as chat, TurnRole, TurnStatus};
use session_core::i18n::{self, Lang, t, t_args};

use super::raw_path_segment;
use aiplane_core::server::db::a2a_contexts::{self, A2aContext, NewContext};
use aiplane_core::server::db::agent_audit::{self, AuditKind};
use aiplane_core::server::db::agents::{self as agents_db, AgentRow};
use aiplane_core::server::principal::{GrantKind, Principal};
use aiplane_runtime::agents::a2a::{self as a2a_rt, CardFacts, TaskState};
use aiplane_runtime::agents::embed::{self as embed_rt, Admission, OpenedTurn, Refusal};
use aiplane_runtime::agents::resume::{AgentResume, AgentResumeError, ResumedBy, claim};
use aiplane_runtime::rama_server::auth::require_bearer;
use aiplane_runtime::rama_server::state::RamaState;
use aiplane_runtime::suspend::ResumeRefused;

/// Longest message accepted, in characters — the embed endpoint's limit.
const MAX_MESSAGE_CHARS: usize = 8_000;
/// How often a stream re-reads its task when no claim release woke it — a
/// backstop only; the release of the turn's claim is what ends the wait.
const FALLBACK_POLL: Duration = Duration::from_secs(2);
const CANCEL_POLL: Duration = Duration::from_millis(50);
const KEEPALIVE: Duration = Duration::from_secs(15);
/// A stream closes after this long even if the task still runs; the client
/// polls `GetTask` or calls `SubscribeToTask` again.
const STREAM_LIMIT: Duration = Duration::from_secs(600);
/// How long `CancelTask` waits for a running turn to notice.
const CANCEL_WAIT: Duration = Duration::from_secs(15);
const A2A_DOMAIN: &str = "a2a-protocol.org";
const AIPLANE_DOMAIN: &str = "aiplane.croit.io";

// ---------------------------------------------------------------------------
// JSON-RPC envelope

/// A JSON-RPC error as A2A §9.5 shapes it: `code`, `message`, and a `data`
/// array holding one `google.rpc.ErrorInfo` whose `reason` names the error.
#[derive(Debug)]
struct RpcError {
    code: i64,
    message: String,
    reason: &'static str,
    domain: &'static str,
    metadata: Map<String, Value>,
    http: StatusCode,
    retry_after: Option<i64>,
}

impl RpcError {
    fn new(code: i64, reason: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            reason,
            domain: A2A_DOMAIN,
            metadata: Map::new(),
            http: StatusCode::OK,
            retry_after: None,
        }
    }

    /// An AIplane error with no A2A code of its own: `-32000`, the first
    /// implementation-defined server error, which A2A leaves unassigned.
    fn aiplane(reason: &'static str, message: impl Into<String>) -> Self {
        Self {
            domain: AIPLANE_DOMAIN,
            ..Self::new(-32000, reason, message)
        }
    }

    fn with(mut self, key: &str, value: impl Into<String>) -> Self {
        self.metadata
            .insert(key.to_string(), Value::String(value.into()));
        self
    }

    fn status(mut self, http: StatusCode) -> Self {
        self.http = http;
        self
    }

    fn data(&self) -> Value {
        json!([{
            "@type": "type.googleapis.com/google.rpc.ErrorInfo",
            "reason": self.reason,
            "domain": self.domain,
            "metadata": self.metadata,
        }])
    }
}

/// Largest JSON-RPC request body read from an A2A caller.
const MAX_BODY_BYTES: usize = 1024 * 1024;

fn parse_error(message: impl Into<String>) -> RpcError {
    RpcError::new(-32700, "PARSE_ERROR", message)
}

fn invalid_request(message: impl Into<String>) -> RpcError {
    RpcError::new(-32600, "INVALID_REQUEST", message)
}

fn invalid_params(message: impl Into<String>) -> RpcError {
    RpcError::new(-32602, "INVALID_PARAMS", message)
}

fn internal(err: impl std::fmt::Display) -> RpcError {
    tracing::warn!(error = %err, "a2a: internal error");
    RpcError::new(
        -32603,
        "INTERNAL",
        "the gateway could not complete this request — try again; if it keeps failing, the \
         agent's owner finds the cause in the gateway log",
    )
}

fn task_not_found(id: &str) -> RpcError {
    RpcError::new(
        -32001,
        "TASK_NOT_FOUND",
        format!("there is no task `{id}` of yours on this agent"),
    )
    .with("taskId", id)
}

fn unsupported(message: impl Into<String>) -> RpcError {
    RpcError::new(-32004, "UNSUPPORTED_OPERATION", message)
}

fn content_type(message: impl Into<String>) -> RpcError {
    RpcError::new(-32005, "CONTENT_TYPE_NOT_SUPPORTED", message)
}

fn task_in_progress(task: &str) -> RpcError {
    RpcError::aiplane(
        "TASK_IN_PROGRESS",
        "the agent is still working on this context — wait for the task to finish (GetTask, or \
         SubscribeToTask), then send the next message",
    )
    .with("taskId", task)
}

fn rpc_body(id: &Value, payload: (&str, Value)) -> Value {
    let mut body = json!({ "jsonrpc": "2.0", "id": id });
    body[payload.0] = payload.1;
    body
}

fn json_response(status: StatusCode, body: &Value) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("static JSON-RPC response")
}

fn error_response(id: &Value, e: RpcError) -> Response {
    let error = json!({ "code": e.code, "message": e.message, "data": e.data() });
    let mut resp = json_response(e.http, &rpc_body(id, ("error", error)));
    if e.http == StatusCode::UNAUTHORIZED {
        resp.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static(r#"Bearer realm="aiplane-a2a""#),
        );
    }
    if let Some(secs) = e.retry_after {
        resp.headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from(secs));
    }
    resp
}

fn result_response(id: &Value, result: Value) -> Response {
    json_response(StatusCode::OK, &rpc_body(id, ("result", result)))
}

// ---------------------------------------------------------------------------
// The served agent

/// An agent as A2A serves it: enabled, published, opted in on its live
/// version.
struct Served {
    agent: AgentRow,
    live_version: i64,
    spec: Value,
}

async fn served(state: &RamaState, id: &str) -> Result<Option<Served>, RpcError> {
    let Some(agent) = agents_db::get(&state.db, id).await.map_err(internal)? else {
        return Ok(None);
    };
    if agent.principal.disabled_at.is_some() {
        return Ok(None);
    }
    let Some((live_version, text)) = agents_db::live(&state.db, id).await.map_err(internal)? else {
        return Ok(None);
    };
    let spec: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if !a2a_rt::enabled(&spec) {
        return Ok(None);
    }
    Ok(Some(Served {
        agent,
        live_version,
        spec,
    }))
}

fn endpoint(state: &RamaState, agent_id: &str) -> String {
    format!("{}/a2a/agents/{agent_id}", state.public_url())
}

/// GET /a2a/agents/{id}/agent-card.json
pub async fn card(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let not_found = || {
        json_response(
            StatusCode::NOT_FOUND,
            &json!({ "error": {
                "code": "agent_not_served",
                "message": "no agent is served over A2A at this URL — its owner has to set \
                            `publish.a2a.enabled: true` and publish it",
            } }),
        )
    };
    let Some(id) = raw_path_segment(&req, 1) else {
        return not_found();
    };
    let s = match served(&state, &id).await {
        Ok(Some(s)) => s,
        Ok(None) => return not_found(),
        Err(_) => {
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &json!({ "error": { "code": "internal", "message": "reading the agent failed" } }),
            );
        }
    };
    let endpoint = endpoint(&state, &s.agent.principal.id);
    let card = a2a_rt::agent_card(&CardFacts {
        agent_name: &s.agent.principal.name,
        principal_display: &s.agent.principal.display,
        description: &s.agent.principal.description,
        live_version: s.live_version,
        spec: &s.spec,
        endpoint: &endpoint,
    });
    let mut resp = json_response(StatusCode::OK, &card);
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=300"),
    );
    if let Ok(etag) =
        HeaderValue::from_str(&format!("\"{}-v{}\"", s.agent.principal.id, s.live_version))
    {
        resp.headers_mut().insert(header::ETAG, etag);
    }
    resp
}

// ---------------------------------------------------------------------------
// Who calls

struct Caller {
    id: String,
    name: String,
    token_id: String,
}

async fn authenticate(
    state: &RamaState,
    headers: &HeaderMap,
) -> Result<(Caller, Principal), RpcError> {
    let ctx = match require_bearer(state, headers).await {
        Ok(ctx) => ctx,
        Err(refusal) if refusal.status == StatusCode::UNAUTHORIZED => {
            return Err(RpcError::aiplane(
                "UNAUTHENTICATED",
                "this agent needs `Authorization: Bearer gws_…` — a system token whose principal \
                 holds the `a2a_caller` grant for it; ask the agent's owner for one",
            )
            .status(StatusCode::UNAUTHORIZED));
        }
        Err(refusal) => return Err(internal(refusal.message)),
    };
    match &ctx.principal {
        Principal::System(sp) => Ok((
            Caller {
                id: sp.id.clone(),
                name: sp.name.clone(),
                token_id: ctx.token_id.clone(),
            },
            ctx.principal.clone(),
        )),
        Principal::User { .. } => Err(RpcError::aiplane(
            "PERMISSION_DENIED",
            "a person's token cannot call an agent over A2A — use a system token (gws_…) whose \
             principal holds the `a2a_caller` grant for this agent",
        )
        .status(StatusCode::FORBIDDEN)),
    }
}

/// The A2A protocol version of the request: the `A2A-Version` header, else
/// the query parameter. Empty means 0.3 (A2A §3.6.2).
fn requested_version(req: &Request) -> String {
    if let Some(v) = req
        .headers()
        .get("a2a-version")
        .and_then(|v| v.to_str().ok())
    {
        return v.trim().to_string();
    }
    req.uri()
        .query()
        .and_then(|q| {
            q.split('&').find_map(|pair| {
                let (k, v) = pair.split_once('=')?;
                k.eq_ignore_ascii_case("a2a-version").then(|| v.to_string())
            })
        })
        .unwrap_or_default()
}

fn version_supported(v: &str) -> bool {
    matches!(v, "1" | "1.0")
}

/// Everything a method handler needs.
struct Call {
    state: Arc<RamaState>,
    served: Served,
    caller: Caller,
    ip: Option<String>,
    lang: Lang,
    id: Value,
}

/// POST /a2a/agents/{id}
pub async fn rpc(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let null = Value::Null;
    let Some(agent_id) = raw_path_segment(&req, 0) else {
        return error_response(&null, invalid_request("the URL names no agent"));
    };
    let (caller, principal) = match authenticate(&state, req.headers()).await {
        Ok(c) => c,
        Err(e) => return error_response(&null, e),
    };
    let s = match served(&state, &agent_id).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            let e = RpcError::aiplane(
                "AGENT_NOT_SERVED",
                "no agent is served over A2A at this URL — check the URL on the agent card, or ask \
                 its owner to enable `publish.a2a` and publish",
            )
            .status(StatusCode::NOT_FOUND);
            return error_response(&null, e);
        }
        Err(e) => return error_response(&null, e),
    };
    let granted = match &principal {
        Principal::System(sp) => sp.grants.has(GrantKind::A2aCaller, &s.agent.principal.id),
        Principal::User { .. } => false,
    };
    if !granted {
        let e = RpcError::aiplane(
            "PERMISSION_DENIED",
            format!(
                "`{}` may not call this agent over A2A — a manager of the agent grants it with \
                 POST /api/v0/system-principals/{}/grants {{\"kind\": \"a2a_caller\", \"ref\": \
                 \"{}\"}}",
                caller.name, caller.id, s.agent.principal.id
            ),
        )
        .status(StatusCode::FORBIDDEN);
        return error_response(&null, e);
    }
    let version = requested_version(&req);
    let ip = state.client_ip(&req);
    let lang = Lang::from_request(req.headers());
    let bytes = match session_core::chrome::read_body_capped(req.into_body(), MAX_BODY_BYTES).await
    {
        Ok(b) => b,
        Err(session_core::chrome::CappedBodyError::TooLarge { max }) => {
            return error_response(
                &null,
                RpcError::aiplane(
                    "PAYLOAD_TOO_LARGE",
                    format!(
                        "the request body is larger than {}; send a smaller message",
                        super::human_size(max)
                    ),
                )
                .status(StatusCode::PAYLOAD_TOO_LARGE),
            );
        }
        Err(session_core::chrome::CappedBodyError::Read(err)) => {
            return error_response(&null, parse_error(err));
        }
    };
    let body: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(err) => {
            return error_response(&null, parse_error(format!("the body is not JSON: {err}")));
        }
    };
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = body
        .get("method")
        .and_then(Value::as_str)
        .filter(|_| body.get("jsonrpc") == Some(&json!("2.0")))
    else {
        return error_response(
            &id,
            invalid_request(
                "send one JSON-RPC 2.0 request object: {\"jsonrpc\": \"2.0\", \"id\": …, \
                 \"method\": \"SendMessage\", \"params\": {…}} (batches are not accepted)",
            ),
        );
    };
    if !version_supported(&version) {
        let shown = if version.is_empty() {
            "0.3 (no A2A-Version header)"
        } else {
            &version
        };
        let e = RpcError::new(
            -32009,
            "VERSION_NOT_SUPPORTED",
            format!(
                "this agent speaks A2A {} only, and the request asked for {shown} — send the \
                 header `A2A-Version: {}`",
                a2a_rt::PROTOCOL_VERSION,
                a2a_rt::PROTOCOL_VERSION
            ),
        )
        .with("supportedVersion", a2a_rt::PROTOCOL_VERSION);
        return error_response(&id, e);
    }
    let params = body.get("params").cloned().unwrap_or_else(|| json!({}));
    let call = Call {
        state,
        served: s,
        caller,
        ip,
        lang,
        id: id.clone(),
    };
    let outcome = match method {
        "SendMessage" => send_message(&call, &params, false).await,
        "SendStreamingMessage" => send_message(&call, &params, true).await,
        "GetTask" => get_task(&call, &params).await,
        "CancelTask" => cancel_task(&call, &params).await,
        "SubscribeToTask" => subscribe(&call, &params).await,
        "ListTasks" | "GetExtendedAgentCard" => Err(unsupported(format!(
            "`{method}` is not offered by this agent (see its agent card's capabilities); \
             keep the task ids SendMessage returns and use GetTask"
        ))),
        "CreateTaskPushNotificationConfig"
        | "GetTaskPushNotificationConfig"
        | "ListTaskPushNotificationConfigs"
        | "DeleteTaskPushNotificationConfig" => Err(RpcError::new(
            -32003,
            "PUSH_NOTIFICATION_NOT_SUPPORTED",
            "this agent sends no push notifications — poll GetTask or use SendStreamingMessage",
        )),
        other => Err(RpcError::new(
            -32601,
            "METHOD_NOT_FOUND",
            format!(
                "`{other}` is not an A2A {} method — use SendMessage, SendStreamingMessage, \
                 GetTask, CancelTask or SubscribeToTask",
                a2a_rt::PROTOCOL_VERSION
            ),
        )),
    };
    match outcome {
        Ok(resp) => resp,
        Err(e) => error_response(&id, e),
    }
}

// ---------------------------------------------------------------------------
// Parameters

#[derive(Debug)]
struct SendParams {
    text: String,
    context_id: Option<String>,
    task_id: Option<String>,
    return_immediately: bool,
    history_length: Option<usize>,
}

fn opt_string(v: &Value, key: &str, at: &str) -> Result<Option<String>, RpcError> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s.trim().to_string())),
        Some(_) => Err(invalid_params(format!(
            "`{at}.{key}` must be a non-empty string"
        ))),
    }
}

fn history_length(v: Option<&Value>, at: &str) -> Result<Option<usize>, RpcError> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(n) => n
            .as_u64()
            .map(|n| Some(n as usize))
            .ok_or_else(|| invalid_params(format!("`{at}` must be a whole number of 0 or more"))),
    }
}

fn accepts_text(mode: &str) -> bool {
    matches!(mode, "text/plain" | "text/*" | "*/*")
}

fn parse_send(params: &Value) -> Result<SendParams, RpcError> {
    let Some(message) = params.get("message").filter(|m| m.is_object()) else {
        return Err(invalid_params(
            "`params.message` is required: the message to send",
        ));
    };
    if message.get("role").and_then(Value::as_str) != Some("ROLE_USER") {
        return Err(invalid_params(
            "`message.role` must be `ROLE_USER`: the caller's message to the agent",
        ));
    }
    if opt_string(message, "messageId", "message")?.is_none() {
        return Err(invalid_params("`message.messageId` is required"));
    }
    let Some(parts) = message
        .get("parts")
        .and_then(Value::as_array)
        .filter(|p| !p.is_empty())
    else {
        return Err(invalid_params(
            "`message.parts` needs at least one text part",
        ));
    };
    let mut texts = Vec::with_capacity(parts.len());
    for (i, part) in parts.iter().enumerate() {
        if ["raw", "url", "data"]
            .iter()
            .any(|k| part.get(*k).is_some())
        {
            return Err(content_type(format!(
                "`message.parts[{i}]` is not text — this agent accepts only text parts \
                 (`{{\"text\": \"…\"}}`, defaultInputModes `text/plain`)"
            )));
        }
        if let Some(media) = part.get("mediaType").and_then(Value::as_str)
            && !media.starts_with("text/")
        {
            return Err(content_type(format!(
                "`message.parts[{i}].mediaType` is `{media}` — this agent accepts text/plain"
            )));
        }
        let Some(text) = part.get("text").and_then(Value::as_str) else {
            return Err(invalid_params(format!(
                "`message.parts[{i}]` needs a `text` string"
            )));
        };
        texts.push(text);
    }
    let text = texts.join("\n").trim().to_string();
    if text.is_empty() {
        return Err(invalid_params(
            "the message is empty — put the text in a part's `text`",
        ));
    }
    if text.chars().count() > MAX_MESSAGE_CHARS {
        return Err(invalid_params(format!(
            "the message is longer than {MAX_MESSAGE_CHARS} characters — shorten it and send it \
             again"
        )));
    }
    let config = params.get("configuration").cloned().unwrap_or(Value::Null);
    if config
        .get("taskPushNotificationConfig")
        .is_some_and(|c| !c.is_null())
    {
        return Err(RpcError::new(
            -32003,
            "PUSH_NOTIFICATION_NOT_SUPPORTED",
            "this agent sends no push notifications — leave out `taskPushNotificationConfig`",
        ));
    }
    if let Some(modes) = config.get("acceptedOutputModes").and_then(Value::as_array)
        && !modes.is_empty()
        && !modes.iter().filter_map(Value::as_str).any(accepts_text)
    {
        return Err(content_type(
            "this agent answers in text/plain only — add it to `acceptedOutputModes`",
        ));
    }
    Ok(SendParams {
        text,
        context_id: opt_string(message, "contextId", "message")?,
        task_id: opt_string(message, "taskId", "message")?,
        return_immediately: config.get("returnImmediately") == Some(&Value::Bool(true)),
        history_length: history_length(config.get("historyLength"), "configuration.historyLength")?,
    })
}

fn task_id_param(params: &Value) -> Result<String, RpcError> {
    opt_string(params, "id", "params")?
        .ok_or_else(|| invalid_params("`params.id` is required: the task id"))
}

// ---------------------------------------------------------------------------
// Admission and the audit trail

async fn admit(call: &Call, context: Option<&str>) -> Result<(), RpcError> {
    let who = Admission {
        visitor_id: None,
        a2a_context: context,
        ip: call.ip.as_deref(),
    };
    let agent = &call.served.agent.principal.id;
    match embed_rt::admit(&call.state, agent, who, Timestamp::now()).await {
        Ok(()) => Ok(()),
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
    if let Err(err) = agent_audit::record_run_event(
        &call.state.db,
        AuditKind::A2aTask,
        &call.served.agent.principal.id,
        None,
        detail,
    )
    .await
    {
        tracing::warn!(error = %err, task, "recording an A2A task");
    }
}

// ---------------------------------------------------------------------------
// Tasks

/// Task `task_id` of this agent, if this caller's context holds it: its
/// context and its assistant turn.
async fn find_task(call: &Call, task_id: &str) -> Result<(A2aContext, chat::Turn), RpcError> {
    let db = &call.state.db;
    let agent = &call.served.agent.principal.id;
    let Some(run) = chat::run_session_of_turn(db, task_id)
        .await
        .map_err(internal)?
        .filter(|s| &s.principal_id == agent && s.parent_turn_id.is_none())
    else {
        return Err(task_not_found(task_id));
    };
    let Some(context) = a2a_contexts::get_for_caller(db, agent, &call.caller.id, &run.id)
        .await
        .map_err(internal)?
    else {
        return Err(task_not_found(task_id));
    };
    match chat::get_turn(db, &run.id, task_id)
        .await
        .map_err(internal)?
    {
        Some(turn) if turn.role == TurnRole::Assistant => Ok((context, turn)),
        _ => Err(task_not_found(task_id)),
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
struct TaskView {
    task: Value,
    state: TaskState,
}

async fn task_view(
    state: &RamaState,
    lang: Lang,
    session_id: &str,
    task_id: &str,
    history: Option<usize>,
) -> Result<TaskView, RpcError> {
    let db = &state.db;
    let Some(turn) = chat::get_turn(db, session_id, task_id)
        .await
        .map_err(internal)?
    else {
        return Err(task_not_found(task_id));
    };
    let suspension = match turn.status {
        TurnStatus::Suspended => chat::get_suspension(db, task_id)
            .await
            .map_err(internal)?
            .map(|s| s.view()),
        _ => None,
    };
    let asked = chat::turn_before(db, session_id, turn.seq)
        .await
        .map_err(internal)?
        .filter(|t| t.role == TurnRole::User);
    let held = state.agent_turns.holds(session_id, task_id);
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
        call.lang,
        session_id,
        task_id,
        p.history_length,
    )
    .await?;
    Ok(result_response(&call.id, json!({ "task": view.task })))
}

async fn send_message(call: &Call, params: &Value, streaming: bool) -> Result<Response, RpcError> {
    let p = parse_send(params)?;
    match p.task_id.clone() {
        Some(task) => continue_task(call, &p, &task, streaming).await,
        None => start_task(call, &p, streaming).await,
    }
}

fn runtime_unavailable() -> RpcError {
    RpcError::aiplane(
        "AGENT_RUNTIME_UNAVAILABLE",
        "this gateway cannot run agent conversations — nothing was stored; try again after the \
         gateway has been updated",
    )
}

async fn start_task(call: &Call, p: &SendParams, streaming: bool) -> Result<Response, RpcError> {
    let state = &call.state;
    let agent = &call.served.agent.principal.id;
    let existing = match &p.context_id {
        Some(id) => Some(
            a2a_contexts::get_for_caller(&state.db, agent, &call.caller.id, id)
                .await
                .map_err(internal)?
                .ok_or_else(|| {
                    invalid_params(format!(
                        "`message.contextId` `{id}` is not a context of yours on this agent — \
                         leave it out to start a new one"
                    ))
                })?,
        ),
        None => None,
    };
    admit(call, existing.as_ref().map(|c| c.session_id.as_str())).await?;
    let Some(runner) = state.agent_turns.runner() else {
        return Err(runtime_unavailable());
    };
    let context = match existing {
        Some(c) => c,
        None => a2a_contexts::open(
            &state.db,
            &NewContext {
                agent_id: agent,
                agent_version: call.served.live_version,
                caller_id: &call.caller.id,
                caller_name: &call.caller.name,
                token_id: &call.caller.token_id,
                client_ip: call.ip.as_deref(),
                now: Timestamp::now(),
            },
        )
        .await
        .map_err(internal)?,
    };
    let session_id = context.session_id.clone();
    let user_turn = uuid::Uuid::new_v4().to_string();
    let task_id = uuid::Uuid::new_v4().to_string();
    let Some(hold) = state.agent_turns.claim(&session_id, &task_id) else {
        return Err(task_in_progress(""));
    };
    if let Some(busy) = chat::in_flight_turn(&state.db, &session_id)
        .await
        .map_err(internal)?
    {
        return Err(task_in_progress(&busy.id));
    }
    if let Some(waiting) = chat::suspended_turn_in_session(&state.db, &session_id)
        .await
        .map_err(internal)?
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
    let version = chat::get_principal_session(&state.db, agent, &session_id)
        .await
        .map_err(internal)?
        .and_then(|run| run.agent_version)
        .unwrap_or(call.served.live_version);
    let model = agents_db::version(&state.db, agent, version)
        .await
        .map_err(internal)?
        .and_then(|row| serde_json::from_str::<Value>(&row.spec).ok())
        .and_then(|spec| {
            spec.pointer("/main/pool")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();

    chat::create_user_turn(&state.db, &session_id, &user_turn, &p.text)
        .await
        .map_err(internal)?;
    chat::create_assistant_turn_in_progress(&state.db, &session_id, &task_id, &model)
        .await
        .map_err(internal)?;
    audit(call, "message", &context, &task_id).await;

    let turn = OpenedTurn {
        agent_id: agent.clone(),
        version,
        session_id: session_id.clone(),
        turn_id: task_id.clone(),
        visitor_id: None,
        caller: Some(context.caller()),
        lang: Some(call.lang),
    };
    let run = {
        let state = state.clone();
        let session_id = session_id.clone();
        let task_id = task_id.clone();
        tokio::spawn(async move {
            let _hold = hold;
            let inner = tokio::spawn({
                let state = state.clone();
                async move { runner.run(state, turn).await }
            });
            if let Err(err) = inner.await {
                tracing::error!(error = %err, task = %task_id, "A2A task runner panicked");
            }
            super::embed::settle_unfinished(&state, &session_id, &task_id).await;
        })
    };
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
        return Err(invalid_params(format!(
            "task `{task_id}` belongs to context `{}`, not `{asked}` — send the task's own \
             contextId or leave it out",
            context.session_id
        )));
    }
    let session_id = context.session_id.clone();
    match TaskState::of(turn.status, state.agent_turns.holds(&session_id, task_id)) {
        TaskState::Working => return Err(task_in_progress(task_id)),
        s if s.is_terminal() => {
            return Err(unsupported(format!(
                "task `{task_id}` is {} and takes no further message — send the message with \
                 only `contextId` to start a new task in the same context",
                s.as_str()
            )));
        }
        _ => {}
    }
    admit(call, Some(&session_id)).await?;
    let Some(runner) = state.agent_turns.runner() else {
        return Err(runtime_unavailable());
    };
    let Some(hold) = state.agent_turns.claim(&session_id, task_id) else {
        return Err(task_in_progress(task_id));
    };
    let decision = super::chat::json_api::decision_from(
        chat::DecisionKind::Value,
        Some(Value::String(p.text.clone())),
    )
    .map_err(invalid_params)?;
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
    let run = {
        let state = state.clone();
        let task = task_id.to_string();
        let session_id = session_id.clone();
        tokio::spawn(async move {
            let _hold = hold;
            let inner = tokio::spawn({
                let state = state.clone();
                async move { runner.resume(state, claimed).await }
            });
            if let Err(err) = inner.await {
                tracing::error!(error = %err, task = %task, "A2A task resume panicked");
            }
            super::embed::settle_unfinished(&state, &session_id, &task).await;
        })
    };
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
        AgentResumeError::Db(err) => internal(err),
        other => invalid_params(other.to_string()),
    }
}

async fn get_task(call: &Call, params: &Value) -> Result<Response, RpcError> {
    let id = task_id_param(params)?;
    let history = history_length(params.get("historyLength"), "params.historyLength")?;
    let (context, _) = find_task(call, &id).await?;
    let view = task_view(&call.state, call.lang, &context.session_id, &id, history).await?;
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

async fn cancel_task(call: &Call, params: &Value) -> Result<Response, RpcError> {
    let id = task_id_param(params)?;
    let (context, turn) = find_task(call, &id).await?;
    let session_id = &context.session_id;
    let turns = &call.state.agent_turns;
    match TaskState::of(turn.status, turns.holds(session_id, &id)) {
        s if s.is_terminal() => return Err(not_cancelable(&id, s)),
        TaskState::Working => {
            if !turns.cancel(session_id, &id) {
                return Err(not_cancelable(&id, TaskState::Working));
            }
            let started = tokio::time::Instant::now();
            while turns.holds(session_id, &id) && started.elapsed() < CANCEL_WAIT {
                tokio::time::sleep(CANCEL_POLL).await;
            }
        }
        _ => {
            let Some(_hold) = turns.claim(session_id, &id) else {
                return Err(task_in_progress(&id));
            };
            cancel_paused(call, &id).await?;
        }
    }
    audit(call, "cancel", &context, &id).await;
    let view = task_view(&call.state, call.lang, session_id, &id, None).await?;
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
            .map_err(internal)?
            .and_then(|s| s.child_turn);
        chat::cancel_suspended_turn(db, &turn)
            .await
            .map_err(internal)?;
        depth += 1;
        if depth < aiplane_core::server::run_chain::MAX_DEPTH {
            next = child;
        }
    }
    Ok(())
}

async fn subscribe(call: &Call, params: &Value) -> Result<Response, RpcError> {
    let id = task_id_param(params)?;
    let (context, turn) = find_task(call, &id).await?;
    let state = TaskState::of(
        turn.status,
        call.state.agent_turns.holds(&context.session_id, &id),
    );
    if state.is_terminal() {
        return Err(unsupported(format!(
            "task `{id}` is {} — read it with GetTask",
            state.as_str()
        )));
    }
    stream(call, &context.session_id, &id).await
}

// ---------------------------------------------------------------------------
// Streaming

fn sse_frame(id: &Value, result: Value) -> rama::bytes::Bytes {
    rama::bytes::Bytes::from(format!("data: {}\n\n", rpc_body(id, ("result", result))))
}

/// The task, then — once it is no longer working — its whole answer as one
/// artifact update and its final status; or its status alone when it failed,
/// was cancelled or waits for input. The answer is buffered behind the
/// output filter, so there is nothing to stream token by token.
async fn stream(call: &Call, session_id: &str, task_id: &str) -> Result<Response, RpcError> {
    let releases = call.state.agent_turns.releases(session_id);
    let first = task_view(&call.state, call.lang, session_id, task_id, None).await?;
    let (tx, rx) = rama::futures::channel::mpsc::unbounded();
    let _ = tx.unbounded_send(Ok(sse_frame(&call.id, json!({ "task": first.task }))));
    let tail = Tail {
        state: call.state.clone(),
        lang: call.lang,
        id: call.id.clone(),
        session_id: session_id.to_string(),
        task_id: task_id.to_string(),
    };
    if first.state == TaskState::Working {
        tokio::spawn(async move { tail.run(releases, tx).await });
    } else {
        tail.finish(&first, &tx);
    }
    Ok(json_stream_response(rx))
}

struct Tail {
    state: Arc<RamaState>,
    lang: Lang,
    id: Value,
    session_id: String,
    task_id: String,
}

impl Tail {
    async fn view(&self) -> Result<TaskView, RpcError> {
        task_view(
            &self.state,
            self.lang,
            &self.session_id,
            &self.task_id,
            None,
        )
        .await
    }

    async fn run(self, mut releases: aiplane_runtime::agents::embed::ReleaseWatch, tx: SseTx) {
        let started = tokio::time::Instant::now();
        let mut last_sent = started;
        loop {
            super::embed::await_release(&mut releases, FALLBACK_POLL).await;
            if tx.is_closed() {
                return;
            }
            match self.view().await {
                Ok(v) if v.state != TaskState::Working => {
                    self.finish(&v, &tx);
                    return;
                }
                Ok(_) => {}
                Err(err) => {
                    tracing::warn!(error = ?err, task = %self.task_id, "a2a stream: reading the task");
                    return;
                }
            }
            if started.elapsed() >= STREAM_LIMIT {
                return;
            }
            if last_sent.elapsed() >= KEEPALIVE {
                let _ = tx.unbounded_send(Ok(rama::bytes::Bytes::from_static(b": working\n\n")));
                last_sent = tokio::time::Instant::now();
            }
        }
    }

    fn finish(&self, v: &TaskView, tx: &SseTx) {
        if let Some(artifact) = v.task.get("artifacts").and_then(|a| a.get(0)) {
            let update = json!({ "artifactUpdate": {
                "taskId": self.task_id,
                "contextId": self.session_id,
                "artifact": artifact,
                "lastChunk": true,
            } });
            let _ = tx.unbounded_send(Ok(sse_frame(&self.id, update)));
        }
        let mut status = json!({ "statusUpdate": {
            "taskId": self.task_id,
            "contextId": self.session_id,
            "status": v.task["status"],
        } });
        if let Some(m) = v.task.get("metadata") {
            status["statusUpdate"]["metadata"] = m.clone();
        }
        let _ = tx.unbounded_send(Ok(sse_frame(&self.id, status)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn send(message: Value) -> Result<SendParams, RpcError> {
        parse_send(&json!({ "message": message }))
    }

    fn msg(parts: Value) -> Value {
        json!({ "messageId": "m1", "role": "ROLE_USER", "parts": parts })
    }

    #[test]
    fn text_parts_are_joined_and_anything_else_is_a_content_type_error() {
        let p = send(msg(
            json!([{ "text": "hello" }, { "text": "world", "mediaType": "text/plain" }]),
        ))
        .unwrap();
        assert_eq!(p.text, "hello\nworld");
        assert!(!p.return_immediately);
        for part in [
            json!({ "url": "https://x/a.png" }),
            json!({ "raw": "AAAA" }),
            json!({ "data": { "a": 1 } }),
            json!({ "text": "x", "mediaType": "application/json" }),
        ] {
            assert_eq!(send(msg(json!([part]))).unwrap_err().code, -32005);
        }
    }

    #[test]
    fn a_message_needs_its_id_the_user_role_and_some_text() {
        assert_eq!(
            send(json!({ "role": "ROLE_USER", "parts": [{ "text": "x" }] }))
                .unwrap_err()
                .code,
            -32602
        );
        let mut agent = msg(json!([{ "text": "x" }]));
        agent["role"] = json!("ROLE_AGENT");
        assert_eq!(send(agent).unwrap_err().code, -32602);
        assert_eq!(send(msg(json!([]))).unwrap_err().code, -32602);
        assert_eq!(
            send(msg(json!([{ "text": "  " }]))).unwrap_err().code,
            -32602
        );
        let long = "x".repeat(MAX_MESSAGE_CHARS + 1);
        assert_eq!(
            send(msg(json!([{ "text": long }]))).unwrap_err().code,
            -32602
        );
    }

    #[test]
    fn configuration_is_honoured_or_refused() {
        let p = parse_send(&json!({
            "message": msg(json!([{ "text": "x" }])),
            "configuration": { "returnImmediately": true, "historyLength": 2,
                               "acceptedOutputModes": ["application/json", "text/plain"] }
        }))
        .unwrap();
        assert!(p.return_immediately);
        assert_eq!(p.history_length, Some(2));
        let push = parse_send(&json!({
            "message": msg(json!([{ "text": "x" }])),
            "configuration": { "taskPushNotificationConfig": { "url": "https://x" } }
        }));
        assert_eq!(push.unwrap_err().code, -32003);
        let json_only = parse_send(&json!({
            "message": msg(json!([{ "text": "x" }])),
            "configuration": { "acceptedOutputModes": ["application/json"] }
        }));
        assert_eq!(json_only.unwrap_err().code, -32005);
    }

    #[test]
    fn only_version_one_zero_is_spoken() {
        assert!(version_supported("1.0"));
        assert!(version_supported("1"));
        for no in ["", "0.3", "1.1", "2.0", "1.0.0"] {
            assert!(!version_supported(no), "{no}");
        }
    }

    #[test]
    fn an_error_carries_its_reason_as_error_info() {
        let e = task_not_found("t1");
        assert_eq!(e.code, -32001);
        let data = e.data();
        assert_eq!(data[0]["@type"], "type.googleapis.com/google.rpc.ErrorInfo");
        assert_eq!(data[0]["reason"], "TASK_NOT_FOUND");
        assert_eq!(data[0]["metadata"]["taskId"], "t1");
        assert_eq!(RpcError::aiplane("RATE_LIMITED", "x").code, -32000);
    }

    #[test]
    fn timestamps_are_utc_with_milliseconds() {
        let t: Timestamp = "2026-10-02T08:09:10.123456Z".parse().unwrap();
        assert_eq!(timestamp(t), "2026-10-02T08:09:10.123Z");
    }
}
