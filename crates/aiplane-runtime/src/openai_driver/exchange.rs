// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! One model round of an agent run in the activity log (`llm_exchange`):
//! the request body exactly as sent and the answer as the stream assembled
//! it. A person's turn records nothing here — their chat keeps its own
//! history and its logging is unchanged.

use std::time::Instant;

use aiplane_agents::db::agent_audit::AuditKind;
use serde_json::{Value, json};
use session_core::driver::TurnError;

use super::OpenAiDriver;
use crate::agents::audit::Redaction;
use crate::server::tools::{ToolContext, ToolSource};

/// Which backend took the request.
pub(super) struct Served<'a> {
    pub model: &'a str,
    pub real_model: &'a str,
    pub backend: Option<&'a str>,
}

/// What came back, assembled from the stream.
#[derive(Default)]
pub(super) struct Answer {
    pub status: Option<u16>,
    pub content: String,
    pub reasoning: String,
    pub tool_calls: Vec<Value>,
    pub finish_reason: Option<String>,
    pub usage: (Option<i64>, Option<i64>, Option<i64>),
    /// The upstream's or the transport's failure, in full.
    pub error: Option<String>,
    pub cancelled: bool,
}

impl Answer {
    pub fn failed(status: Option<u16>, error: impl Into<String>) -> Self {
        Self {
            status,
            error: Some(error.into()),
            ..Self::default()
        }
    }

    pub fn from_error(err: &TurnError) -> Self {
        Self::failed(None, err.to_string())
    }
}

/// Record the vision fallback's calls for the result of tool call
/// `call_id`, in an agent run only.
pub(super) async fn record_vision_fallback(
    d: &OpenAiDriver,
    tool_ctx: &ToolContext,
    call_id: &str,
    calls: &[aiplane_core::server::capabilities::DescribeCall],
) {
    if d.agent().is_none() || calls.is_empty() {
        return;
    }
    let ctx = ToolContext {
        call_id: Some(call_id.to_string()),
        ..tool_ctx.clone()
    };
    for call in calls {
        let mut detail = json!({
            "purpose": "vision_fallback",
            "model": call.model,
            "backend": call.backend,
            "request": call.request,
            "response": { "status": call.status, "body": call.response },
            "latency_ms": call.latency_ms,
        });
        if let Some(error) = &call.error {
            detail["error"] = json!(error);
        }
        ctx.audit_event(AuditKind::LlmExchange, Some(call.latency_ms), detail)
            .await;
    }
}

/// The rounds of one turn, as the log keeps them.
pub(super) struct ExchangeLog<'a> {
    /// The turn's tools: which of them declare their arguments sensitive.
    pub tools: &'a dyn ToolSource,
    pub redaction: Redaction<'a>,
}

impl ExchangeLog<'_> {
    /// Record one round. `request` is the body that went (or would have
    /// gone) upstream; `started` is when it was sent.
    #[allow(clippy::too_many_arguments)]
    pub async fn record(
        &self,
        d: &OpenAiDriver,
        tool_ctx: &ToolContext,
        round: u32,
        request: &Value,
        served: Served<'_>,
        answer: Answer,
        started: Instant,
    ) {
        if d.agent().is_none() {
            return;
        }
        let mut request = request.clone();
        let mut answer = answer;
        self.redaction
            .exchange(&mut request, &mut answer.tool_calls, |name| {
                self.tools.get(name).is_some_and(|t| t.sensitive_args())
            });
        record(tool_ctx, round, &request, served, answer, started).await;
    }
}

async fn record(
    tool_ctx: &ToolContext,
    round: u32,
    request: &Value,
    served: Served<'_>,
    answer: Answer,
    started: Instant,
) {
    let (prompt, completion, total) = answer.usage;
    let latency = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let ctx = ToolContext {
        call_id: None,
        ..tool_ctx.clone()
    };
    let mut event = json!({
        "purpose": "round",
        "round": round,
        "model": served.model,
        "real_model": served.real_model,
        "backend": served.backend,
        "request": request,
        "response": {
            "status": answer.status,
            "content": answer.content,
            "reasoning": answer.reasoning,
            "tool_calls": answer.tool_calls,
            "finish_reason": answer.finish_reason,
            "usage": {
                "prompt_tokens": prompt,
                "completion_tokens": completion,
                "total_tokens": total,
            },
        },
        "latency_ms": latency,
    });
    if let Some(error) = answer.error {
        event["error"] = json!(error);
    }
    if answer.cancelled {
        event["cancelled"] = json!(true);
    }
    ctx.audit_event(AuditKind::LlmExchange, Some(latency), event)
        .await;
}
