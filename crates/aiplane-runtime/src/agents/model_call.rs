// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! One model call whose answer is a JSON object of a given shape, on one
//! model ([`ask_json`]): the mechanism behind the route classifier
//! ([`super::router::ModelClassifier`]), the topic guard
//! ([`super::topic_guard`]), the evaluation judge
//! ([`super::eval_judge`]) and the prompt assistant ([`super::assist`]).
//!
//! The answer is constrained twice: the request's `response_format` names
//! the schema (for a choice, the allowed values as an enum), and the caller
//! still checks what comes back in code, because a backend may ignore the
//! format. The model is resolved the way a chat turn resolves it — an
//! automatic route's alias by its selector ([`route_target`]) — and reached
//! under the access it is given. Whose usage row it is, and where it is
//! recorded, is the caller's: [`ModelCall`] makes it a usage row of the
//! agent's run (so it counts against the owner's budget and the model's
//! limits) and an `llm_exchange` in its activity log.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aiplane_agents::db::agent_audit::AuditKind;
use aiplane_core::server::capped_read;
use aiplane_core::server::db::usage::{UsageKind, UsageRecord, UsageSource, usage_from_value};
use aiplane_core::server::principal::{PrincipalKind, SystemPrincipal};
use aiplane_core::server::run_chain::RunChain;
use aiplane_core::server::upstreams::{PoolAccess, PoolKind};
use serde_json::{Value, json};

use crate::rama_server::state::RamaState;
use crate::server::model_route::route_target;
use crate::server::tools::ToolContext;

const CHOICE_TIMEOUT: Duration = Duration::from_secs(30);

/// What to ask: the instructions, the input (data, never instructions), and
/// the JSON field whose value must be one of `choices`.
pub struct Question<'a> {
    /// The `purpose` its `llm_exchange` carries.
    pub purpose: &'static str,
    pub instructions: &'a str,
    pub input: String,
    pub field: &'static str,
    pub choices: &'a [&'a str],
}

/// What came back: the value the model gave for the field (not yet checked
/// against the choices), or why there is none, and the tokens the call
/// spent.
pub struct Choice {
    pub answer: Result<String, String>,
    pub tokens: u64,
}

/// A model of an agent's, ready to be asked.
pub struct ModelCall {
    state: Arc<RamaState>,
    model: String,
    access: PoolAccess,
    principal: SystemPrincipal,
    run: Option<Arc<RunChain>>,
    /// Where the exchange is recorded: the call or turn it decides for.
    log: ToolContext,
}

impl ModelCall {
    /// `principal`'s model `model`, asked on behalf of the run `log`
    /// belongs to. It reaches the model only when the principal holds a
    /// grant on it.
    pub fn new(
        state: Arc<RamaState>,
        model: &str,
        principal: &SystemPrincipal,
        log: &ToolContext,
    ) -> Self {
        Self {
            access: PoolAccess::for_system_models(principal, [model]),
            model: model.to_string(),
            principal: principal.clone(),
            run: log.agent.as_ref().map(|a| a.chain().clone()),
            log: log.clone(),
            state,
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub async fn ask(&self, question: Question<'_>) -> Choice {
        let field = question.field;
        let exchange = ask_json(
            &self.state,
            &self.model,
            &self.access,
            &JsonQuestion {
                instructions: question.instructions,
                input: &question.input,
                name: field,
                schema: json!({
                    "type": "object",
                    "properties": { field: { "type": "string", "enum": question.choices } },
                    "required": [field],
                    "additionalProperties": false,
                }),
                temperature: 0.0,
                timeout: CHOICE_TIMEOUT,
            },
        )
        .await;
        let tokens = self.record(&exchange);
        let answer = exchange.answer.clone().and_then(|a| {
            a.get(field)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("the answer names no `{field}`"))
        });
        self.log_exchange(&question, &exchange, &answer).await;
        Choice { answer, tokens }
    }

    async fn log_exchange(
        &self,
        question: &Question<'_>,
        exchange: &JsonExchange,
        answer: &Result<String, String>,
    ) {
        let mut detail = json!({
            "purpose": question.purpose,
            "model": exchange.model,
            "backend": exchange.backend,
            "request": exchange.request,
            "response": { "status": exchange.status, "body": exchange.response },
            "answer": answer.as_ref().ok(),
            "latency_ms": exchange.latency_ms,
        });
        if let Err(error) = answer {
            detail["error"] = json!(error);
        }
        self.log
            .audit_event(AuditKind::LlmExchange, Some(exchange.latency_ms), detail)
            .await;
    }

    /// Emit the call's usage row, when it reached a backend; the tokens it
    /// spent.
    fn record(&self, exchange: &JsonExchange) -> u64 {
        let Some(usage) = exchange.usage_record(
            &self.state,
            &self.principal.id,
            Some(self.principal.name.clone()),
            UsageSource::Agent,
            PrincipalKind::System,
        ) else {
            return 0;
        };
        let tokens = usage
            .total_tokens
            .and_then(|t| u64::try_from(t).ok())
            .unwrap_or(0);
        if self.state.usage.is_enabled() {
            self.state.usage.emit(usage.in_run(self.run.as_deref()));
        }
        tokens
    }
}

/// One non-streaming request whose answer must be a JSON object matching
/// `schema` (`response_format: json_schema`, strict).
pub struct JsonQuestion<'a> {
    pub instructions: &'a str,
    /// Data for the model, never instructions.
    pub input: &'a str,
    /// The schema's name in the request.
    pub name: &'a str,
    pub schema: Value,
    pub temperature: f64,
    pub timeout: Duration,
}

/// What an [`ask_json`] call did: enough for a usage row and an activity
/// event, and the answer object or why there is none.
#[derive(Debug, Clone)]
pub struct JsonExchange {
    pub model: Option<String>,
    pub backend: Option<String>,
    pub request: Value,
    pub status: Option<u16>,
    /// The upstream's whole answer, `null` when none was read.
    pub response: Value,
    pub latency_ms: u64,
    pub answer: Result<Value, String>,
}

impl JsonExchange {
    /// The call's usage row for `user_id`, when it reached a backend. The
    /// caller decides whose run it belongs to and emits it.
    pub fn usage_record(
        &self,
        state: &RamaState,
        user_id: &str,
        user_email: Option<String>,
        source: UsageSource,
        principal_kind: PrincipalKind,
    ) -> Option<UsageRecord> {
        let (status, model, backend) = (self.status?, self.model.as_ref()?, self.backend.as_ref()?);
        let (prompt_tokens, completion_tokens, total_tokens) = usage_from_value(&self.response);
        Some(UsageRecord {
            created_at: jiff::Timestamp::now(),
            user_id: user_id.to_string(),
            user_email,
            token_id: None,
            token_name: None,
            source,
            kind: UsageKind::Chat,
            backend: backend.clone(),
            model: model.clone(),
            status,
            duration_ms: i64::try_from(self.latency_ms).unwrap_or(i64::MAX),
            prompt_tokens,
            completion_tokens,
            total_tokens,
            input_units: None,
            output_units: None,
            enforce_limits: state
                .upstreams
                .enforce_limits_for_model(model, PoolKind::Chat),
            principal_kind,
            agent_id: None,
            chain: None,
        })
    }
}

/// Ask model `model`, under `access`, for a JSON object matching
/// `question.schema`. The schema is a request, not a guarantee — a backend
/// may ignore `response_format` — so the caller still checks what it reads.
pub async fn ask_json(
    state: &RamaState,
    model: &str,
    access: &PoolAccess,
    question: &JsonQuestion<'_>,
) -> JsonExchange {
    let started = Instant::now();
    let mut exchange = JsonExchange {
        model: None,
        backend: None,
        request: Value::Null,
        status: None,
        response: Value::Null,
        latency_ms: 0,
        answer: Err(String::new()),
    };
    exchange.answer = call(state, model, access, question, &mut exchange).await;
    exchange.latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    exchange
}

async fn call(
    state: &RamaState,
    model: &str,
    access: &PoolAccess,
    question: &JsonQuestion<'_>,
    exchange: &mut JsonExchange,
) -> Result<Value, String> {
    let messages = json!([
        {"role": "system", "content": question.instructions},
        {"role": "user", "content": question.input},
    ]);
    let target = route_target(
        state,
        model,
        &json!({ "messages": messages, "tools": [] }),
        access,
        None,
    )
    .await
    .map_err(|e| e.to_string())?;
    let acquired = state
        .upstreams
        .route_access(&target.model, PoolKind::Chat, &target.access)
        .map_err(|e| e.to_string())?;
    let backend = acquired.backend();
    let body = json!({
        "model": acquired.resolved_model(),
        "messages": messages,
        "temperature": question.temperature,
        "stream": false,
        // A reasoning model can spend the whole answer thinking and return no
        // content; the title and compaction calls switch it off the same way.
        "chat_template_kwargs": { "enable_thinking": false },
        "response_format": {
            "type": "json_schema",
            "json_schema": { "name": question.name, "strict": true, "schema": question.schema },
        },
    });
    exchange.model = Some(target.model);
    exchange.backend = Some(backend.name.clone());
    exchange.request = body.clone();
    let mut req = state
        .http
        .post(format!("{}/chat/completions", backend.base_url))
        .timeout(question.timeout)
        .json(&body);
    if let Some(key) = backend.api_key.as_deref() {
        req = req.bearer_auth(key);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    exchange.status = Some(status.as_u16());
    if !status.is_success() {
        return Err(format!("upstream {status}"));
    }
    let parsed: Value = capped_read::read_capped_json(resp, capped_read::MODEL_ANSWER_BYTES)
        .await
        .map_err(|e| e.to_string())?;
    drop(acquired);
    exchange.response = parsed;
    let content = exchange
        .response
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or("the answer has no content")?;
    let trimmed = content
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let answer: Value =
        serde_json::from_str(trimmed).map_err(|e| format!("the answer is not JSON ({e})"))?;
    if answer.is_object() {
        Ok(answer)
    } else {
        Err("the answer is not a JSON object".into())
    }
}
