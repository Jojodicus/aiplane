// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! One model call that picks an answer from a fixed set, on one pool of an
//! agent's: the mechanism behind the route classifier
//! ([`super::router::PoolClassifier`]) and the topic guard
//! ([`super::topic_guard`]).
//!
//! The answer is constrained twice: the request's `response_format` names
//! the allowed values as an enum, and the caller still checks what comes
//! back in code, because a backend may ignore the format. The call reaches
//! only the pool it is given, under the agent principal's pool grant; it is
//! a usage row of the agent's run (so it counts against the owner's budget
//! and the pool's limits) and an `llm_exchange` in its activity log.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aiplane_agents::db::agent_audit::AuditKind;
use aiplane_core::server::capped_read;
use aiplane_core::server::db::usage::{UsageKind, UsageRecord, UsageSource, usage_from_value};
use aiplane_core::server::principal::{PrincipalKind, SystemPrincipal};
use aiplane_core::server::run_chain::RunChain;
use aiplane_core::server::upstreams::{PoolAccess, PoolKind};
use serde_json::{Value, json};

use super::profile::pool_model;
use crate::rama_server::state::RamaState;
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

/// A pool of an agent's, ready to be asked.
pub struct PoolChoice {
    state: Arc<RamaState>,
    pool: String,
    access: PoolAccess,
    principal: SystemPrincipal,
    run: Option<Arc<RunChain>>,
    /// Where the exchange is recorded: the call or turn it decides for.
    log: ToolContext,
}

/// The exchange as the activity log records it.
#[derive(Default)]
struct Exchange {
    model: Option<String>,
    backend: Option<String>,
    request: Value,
    status: Option<u16>,
    response: Value,
}

impl PoolChoice {
    /// `principal`'s pool `pool`, asked on behalf of the run `log` belongs
    /// to.
    pub fn new(
        state: Arc<RamaState>,
        pool: &str,
        principal: &SystemPrincipal,
        log: &ToolContext,
    ) -> Self {
        Self {
            access: PoolAccess::for_system_pools(principal, [pool]),
            pool: pool.to_string(),
            principal: principal.clone(),
            run: log.agent.as_ref().map(|a| a.chain().clone()),
            log: log.clone(),
            state,
        }
    }

    pub fn pool(&self) -> &str {
        &self.pool
    }

    pub async fn ask(&self, question: Question<'_>) -> Choice {
        let started = Instant::now();
        let mut exchange = Exchange::default();
        let mut tokens = 0;
        let answer = self
            .call(&question, &mut exchange, &mut tokens, started)
            .await;
        self.log_exchange(&question, exchange, &answer, started)
            .await;
        Choice { answer, tokens }
    }

    async fn log_exchange(
        &self,
        question: &Question<'_>,
        exchange: Exchange,
        answer: &Result<String, String>,
        started: Instant,
    ) {
        let latency = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut detail = json!({
            "purpose": question.purpose,
            "pool": self.pool,
            "model": exchange.model,
            "backend": exchange.backend,
            "request": exchange.request,
            "response": { "status": exchange.status, "body": exchange.response },
            "answer": answer.as_ref().ok(),
            "latency_ms": latency,
        });
        if let Err(error) = answer {
            detail["error"] = json!(error);
        }
        self.log
            .audit_event(AuditKind::LlmExchange, Some(latency), detail)
            .await;
    }

    /// Emit the call's usage row; the tokens it spent.
    fn record(
        &self,
        backend: &str,
        model: &str,
        status: u16,
        started: Instant,
        body: &Value,
    ) -> u64 {
        let (prompt_tokens, completion_tokens, total_tokens) = usage_from_value(body);
        if self.state.usage.is_enabled() {
            self.state.usage.emit(
                UsageRecord {
                    created_at: jiff::Timestamp::now(),
                    user_id: self.principal.id.clone(),
                    user_email: Some(self.principal.name.clone()),
                    token_id: None,
                    token_name: None,
                    source: UsageSource::Scheduled,
                    kind: UsageKind::Chat,
                    backend: backend.to_string(),
                    model: model.to_string(),
                    status,
                    duration_ms: started.elapsed().as_millis() as i64,
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
                    input_units: None,
                    output_units: None,
                    enforce_limits: self
                        .state
                        .upstreams
                        .enforce_limits_for_model(model, PoolKind::Chat),
                    principal_kind: PrincipalKind::System,
                    agent_id: None,
                    chain: None,
                }
                .in_run(self.run.as_deref()),
            );
        }
        total_tokens
            .and_then(|t| u64::try_from(t).ok())
            .unwrap_or(0)
    }

    async fn call(
        &self,
        question: &Question<'_>,
        exchange: &mut Exchange,
        tokens: &mut u64,
        started: Instant,
    ) -> Result<String, String> {
        let model = pool_model(&self.state, &self.pool, &self.access)
            .ok_or_else(|| format!("pool `{}` serves no model it may use", self.pool))?;
        let acquired = self
            .state
            .upstreams
            .route_access(&model, PoolKind::Chat, &self.access)
            .map_err(|e| e.to_string())?;
        let backend = acquired.backend();
        let field = question.field;
        let body = json!({
            "model": acquired.resolved_model(),
            "messages": [
                {"role": "system", "content": question.instructions},
                {"role": "user", "content": question.input},
            ],
            "temperature": 0,
            "stream": false,
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": field,
                    "strict": true,
                    "schema": {
                        "type": "object",
                        "properties": { field: { "type": "string", "enum": question.choices } },
                        "required": [field],
                        "additionalProperties": false,
                    },
                },
            },
        });
        exchange.model = Some(model.clone());
        exchange.backend = Some(backend.name.clone());
        exchange.request = body.clone();
        let mut req = self
            .state
            .http
            .post(format!("{}/chat/completions", backend.base_url))
            .timeout(CHOICE_TIMEOUT)
            .json(&body);
        if let Some(key) = backend.api_key.as_deref() {
            req = req.bearer_auth(key);
        }
        let backend_name = backend.name.clone();
        let resp = req.send().await.map_err(|e| e.to_string())?;
        let status = resp.status();
        exchange.status = Some(status.as_u16());
        if !status.is_success() {
            self.record(
                &backend_name,
                &model,
                status.as_u16(),
                started,
                &Value::Null,
            );
            return Err(format!("upstream {status}"));
        }
        let parsed: Value = capped_read::read_capped_json(resp, capped_read::MODEL_ANSWER_BYTES)
            .await
            .map_err(|e| e.to_string())?;
        drop(acquired);
        *tokens = self.record(&backend_name, &model, status.as_u16(), started, &parsed);
        exchange.response = parsed.clone();
        let content = parsed
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
        answer
            .get(field)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("the answer names no `{field}`"))
    }
}
