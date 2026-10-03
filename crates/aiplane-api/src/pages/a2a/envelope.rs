// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The JSON-RPC envelope: A2A's error shape and the result and error
//! responses around it.

use rama::http::{HeaderValue, Response, StatusCode, header};
use serde_json::{Map, Value, json};

use crate::pages::json_ok;

const A2A_DOMAIN: &str = "a2a-protocol.org";
const AIPLANE_DOMAIN: &str = "aiplane.croit.io";

/// A JSON-RPC error as A2A §9.5 shapes it: `code`, `message`, and a `data`
/// array holding one `google.rpc.ErrorInfo` whose `reason` names the error.
#[derive(Debug)]
pub(super) struct RpcError {
    pub(super) code: i64,
    message: String,
    reason: &'static str,
    domain: &'static str,
    metadata: Map<String, Value>,
    http: StatusCode,
    pub(super) retry_after: Option<i64>,
}

impl RpcError {
    pub(super) fn new(code: i64, reason: &'static str, message: impl Into<String>) -> Self {
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
    pub(super) fn aiplane(reason: &'static str, message: impl Into<String>) -> Self {
        Self {
            domain: AIPLANE_DOMAIN,
            ..Self::new(-32000, reason, message)
        }
    }

    pub(super) fn with(mut self, key: &str, value: impl Into<String>) -> Self {
        self.metadata
            .insert(key.to_string(), Value::String(value.into()));
        self
    }

    pub(super) fn status(mut self, http: StatusCode) -> Self {
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

    pub(super) fn parse_error(message: impl Into<String>) -> Self {
        Self::new(-32700, "PARSE_ERROR", message)
    }

    pub(super) fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(-32600, "INVALID_REQUEST", message)
    }

    pub(super) fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(-32602, "INVALID_PARAMS", message)
    }

    pub(super) fn internal(err: impl std::fmt::Display) -> Self {
        tracing::warn!(error = %err, "a2a: internal error");
        Self::new(
            -32603,
            "INTERNAL",
            "the gateway could not complete this request — try again; if it keeps failing, the \
             agent's owner finds the cause in the gateway log",
        )
    }

    pub(super) fn task_not_found(id: &str) -> Self {
        Self::new(
            -32001,
            "TASK_NOT_FOUND",
            format!("there is no task `{id}` of yours on this agent"),
        )
        .with("taskId", id)
    }

    pub(super) fn unsupported(message: impl Into<String>) -> Self {
        Self::new(-32004, "UNSUPPORTED_OPERATION", message)
    }

    pub(super) fn content_type(message: impl Into<String>) -> Self {
        Self::new(-32005, "CONTENT_TYPE_NOT_SUPPORTED", message)
    }

    pub(super) fn push_not_supported(message: impl Into<String>) -> Self {
        Self::new(-32003, "PUSH_NOTIFICATION_NOT_SUPPORTED", message)
    }

    pub(super) fn task_in_progress(task: &str) -> Self {
        Self::aiplane(
            "TASK_IN_PROGRESS",
            "the agent is still working on this context — wait for the task to finish (GetTask, \
             or SubscribeToTask), then send the next message",
        )
        .with("taskId", task)
    }

    pub(super) fn runtime_unavailable() -> Self {
        Self::aiplane(
            "AGENT_RUNTIME_UNAVAILABLE",
            "this gateway cannot run agent conversations — nothing was stored; try again after \
             the gateway has been updated",
        )
    }
}

pub(super) fn rpc_body(id: &Value, payload: (&str, Value)) -> Value {
    let mut body = json!({ "jsonrpc": "2.0", "id": id });
    body[payload.0] = payload.1;
    body
}

pub(super) fn error_response(id: &Value, e: RpcError) -> Response {
    let error = json!({ "code": e.code, "message": e.message, "data": e.data() });
    let mut resp = json_ok(e.http, rpc_body(id, ("error", error)));
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

pub(super) fn result_response(id: &Value, result: Value) -> Response {
    json_ok(StatusCode::OK, rpc_body(id, ("result", result)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_carries_its_reason_as_error_info() {
        let e = RpcError::task_not_found("t1");
        assert_eq!(e.code, -32001);
        let data = e.data();
        assert_eq!(data[0]["@type"], "type.googleapis.com/google.rpc.ErrorInfo");
        assert_eq!(data[0]["reason"], "TASK_NOT_FOUND");
        assert_eq!(data[0]["metadata"]["taskId"], "t1");
        assert_eq!(RpcError::aiplane("RATE_LIMITED", "x").code, -32000);
    }
}
