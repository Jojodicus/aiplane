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

mod envelope;
mod params;
mod stream;
mod tasks;

use std::sync::Arc;

use rama::http::service::web::extract::State;
use rama::http::{HeaderMap, HeaderValue, Request, Response, StatusCode, header};
use serde_json::{Value, json};
use session_core::i18n::Lang;

use super::{json_error, json_ok, raw_path_segment};
use aiplane_core::server::db::agents::{self as agents_db, AgentRow};
use aiplane_core::server::principal::{GrantKind, Principal};
use aiplane_runtime::agents::a2a::{self as a2a_rt, CardFacts};
use aiplane_runtime::rama_server::auth::require_bearer;
use aiplane_runtime::rama_server::state::RamaState;
use envelope::{RpcError, error_response};

/// Largest JSON-RPC request body read from an A2A caller.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// An agent as A2A serves it: enabled, published, opted in on its live
/// version.
struct Served {
    agent: AgentRow,
    live_version: i64,
    spec: Value,
}

async fn served(state: &RamaState, id: &str) -> Result<Option<Served>, RpcError> {
    let Some(agent) = agents_db::get(&state.db, id)
        .await
        .map_err(RpcError::internal)?
    else {
        return Ok(None);
    };
    if agent.principal.disabled_at.is_some() {
        return Ok(None);
    }
    let Some((live_version, text)) = agents_db::live(&state.db, id)
        .await
        .map_err(RpcError::internal)?
    else {
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
        json_error(
            StatusCode::NOT_FOUND,
            "agent_not_served",
            "no agent is served over A2A at this URL — its owner has to set \
             `publish.a2a.enabled: true` and publish it",
        )
    };
    let Some(id) = raw_path_segment(&req, 1) else {
        return not_found();
    };
    let s = match served(&state, &id).await {
        Ok(Some(s)) => s,
        Ok(None) => return not_found(),
        Err(_) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "reading the agent failed",
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
    let mut resp = json_ok(StatusCode::OK, card);
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
        Err(refusal) => return Err(RpcError::internal(refusal.message)),
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
    match handle(state, req).await {
        Ok(resp) => resp,
        Err((id, e)) => error_response(&id, e),
    }
}

/// The request's response, or the error to answer it with and the request
/// id that error carries: `null` until the body named one.
async fn handle(state: Arc<RamaState>, req: Request) -> Result<Response, (Value, RpcError)> {
    let anonymous = |e: RpcError| (Value::Null, e);
    let agent_id = raw_path_segment(&req, 0)
        .ok_or_else(|| anonymous(RpcError::invalid_request("the URL names no agent")))?;
    let (caller, principal) = authenticate(&state, req.headers())
        .await
        .map_err(anonymous)?;
    let s = served(&state, &agent_id)
        .await
        .map_err(anonymous)?
        .ok_or_else(|| {
            anonymous(
                RpcError::aiplane(
                    "AGENT_NOT_SERVED",
                    "no agent is served over A2A at this URL — check the URL on the agent card, \
                     or ask its owner to enable `publish.a2a` and publish",
                )
                .status(StatusCode::NOT_FOUND),
            )
        })?;
    let granted = match &principal {
        Principal::System(sp) => sp.grants.has(GrantKind::A2aCaller, &s.agent.principal.id),
        Principal::User { .. } => false,
    };
    if !granted {
        return Err(anonymous(
            RpcError::aiplane(
                "PERMISSION_DENIED",
                format!(
                    "`{}` may not call this agent over A2A — a manager of the agent grants it \
                     with POST /api/v0/system-principals/{}/grants {{\"kind\": \"a2a_caller\", \
                     \"ref\": \"{}\"}}",
                    caller.name, caller.id, s.agent.principal.id
                ),
            )
            .status(StatusCode::FORBIDDEN),
        ));
    }
    let version = requested_version(&req);
    let ip = state.client_ip(&req);
    let lang = Lang::from_request(req.headers());
    let bytes = session_core::chrome::read_body_capped(req.into_body(), MAX_BODY_BYTES)
        .await
        .map_err(|err| {
            anonymous(match err {
                session_core::chrome::CappedBodyError::TooLarge { max } => RpcError::aiplane(
                    "PAYLOAD_TOO_LARGE",
                    format!(
                        "the request body is larger than {}; send a smaller message",
                        super::human_size(max)
                    ),
                )
                .status(StatusCode::PAYLOAD_TOO_LARGE),
                session_core::chrome::CappedBodyError::Read(err) => RpcError::parse_error(err),
            })
        })?;
    let body: Value = serde_json::from_slice(&bytes).map_err(|err| {
        anonymous(RpcError::parse_error(format!(
            "the body is not JSON: {err}"
        )))
    })?;
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    let fail = |e: RpcError| (id.clone(), e);
    let Some(method) = body
        .get("method")
        .and_then(Value::as_str)
        .filter(|_| body.get("jsonrpc") == Some(&json!("2.0")))
    else {
        return Err(fail(RpcError::invalid_request(
            "send one JSON-RPC 2.0 request object: {\"jsonrpc\": \"2.0\", \"id\": …, \
             \"method\": \"SendMessage\", \"params\": {…}} (batches are not accepted)",
        )));
    };
    if !version_supported(&version) {
        let shown = if version.is_empty() {
            "0.3 (no A2A-Version header)"
        } else {
            &version
        };
        return Err(fail(
            RpcError::new(
                -32009,
                "VERSION_NOT_SUPPORTED",
                format!(
                    "this agent speaks A2A {} only, and the request asked for {shown} — send the \
                     header `A2A-Version: {}`",
                    a2a_rt::PROTOCOL_VERSION,
                    a2a_rt::PROTOCOL_VERSION
                ),
            )
            .with("supportedVersion", a2a_rt::PROTOCOL_VERSION),
        ));
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
    match method {
        "SendMessage" => tasks::send_message(&call, &params, false).await,
        "SendStreamingMessage" => tasks::send_message(&call, &params, true).await,
        "GetTask" => tasks::get_task(&call, &params).await,
        "CancelTask" => tasks::cancel_task(&call, &params).await,
        "SubscribeToTask" => tasks::subscribe(&call, &params).await,
        "ListTasks" | "GetExtendedAgentCard" => Err(RpcError::unsupported(format!(
            "`{method}` is not offered by this agent (see its agent card's capabilities); keep \
             the task ids SendMessage returns and use GetTask"
        ))),
        "CreateTaskPushNotificationConfig"
        | "GetTaskPushNotificationConfig"
        | "ListTaskPushNotificationConfigs"
        | "DeleteTaskPushNotificationConfig" => Err(RpcError::push_not_supported(
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
    }
    .map_err(fail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_version_one_zero_is_spoken() {
        assert!(version_supported("1.0"));
        assert!(version_supported("1"));
        for no in ["", "0.3", "1.1", "2.0", "1.0.0"] {
            assert!(!version_supported(no), "{no}");
        }
    }
}
