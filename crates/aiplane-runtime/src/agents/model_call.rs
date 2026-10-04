// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! One constrained choice on one of an agent's models ([`ModelCall`]): the
//! mechanism behind the route classifier ([`super::router::ModelClassifier`])
//! and the topic guard ([`super::topic_guard`]).
//!
//! The answer is constrained twice: the request's `response_format` names
//! the allowed values as an enum, and the caller still checks what comes
//! back in code, because a backend may ignore the format. The call itself is
//! a side call ([`side_call::ask_json`]): the model resolved as a chat turn
//! resolves it, the agent's budget checked first, a usage row of the agent's
//! run (so it counts against the owner's budget and the model's limits), and
//! an `llm_exchange` in its activity log.

use std::sync::Arc;
use std::time::Duration;

use aiplane_core::server::principal::SystemPrincipal;
use aiplane_core::server::upstreams::PoolAccess;
use serde_json::{Value, json};

use super::audit::RunLog;
use crate::rama_server::state::RamaState;
use crate::server::side_call::{self, JsonShape, Payer, SideCall};
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
    payer: Payer,
    /// Where the exchange is recorded: the run of the call or turn it
    /// decides for.
    log: Option<RunLog>,
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
            payer: Payer::agent(principal, log.agent.as_ref().map(|a| a.chain().clone())),
            log: RunLog::of(log),
            state,
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub async fn ask(&self, question: Question<'_>) -> Choice {
        let field = question.field;
        let mut answered = side_call::ask_json(
            &self.state,
            &self.payer,
            SideCall {
                purpose: question.purpose,
                model: &self.model,
                access: &self.access,
                instructions: question.instructions,
                input: &question.input,
                temperature: 0.0,
                max_tokens: None,
                no_think: false,
                timeout: CHOICE_TIMEOUT,
            },
            JsonShape {
                name: field,
                schema: json!({
                    "type": "object",
                    "properties": { field: { "type": "string", "enum": question.choices } },
                    "required": [field],
                    "additionalProperties": false,
                }),
            },
        )
        .await;
        let answer = match &answered.answer {
            Ok(a) => a
                .get(field)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("the answer names no `{field}`")),
            Err(e) => Err(e.to_string()),
        };
        answered.exchange.answer = Some(json!(answer.as_ref().ok()));
        if let Err(error) = &answer {
            answered.exchange.error = Some(error.clone());
        }
        if let Some(log) = &self.log {
            log.record(&self.state.db, &answered.exchange).await;
        }
        Choice {
            answer,
            tokens: answered.tokens(),
        }
    }
}
