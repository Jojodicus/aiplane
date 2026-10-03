// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The prompt assistant (`docs/agents.md` → "What #117 built"): from a
//! manager's scenario, one model call proposes a value for every setup step
//! of an agent plus test cases; another improves one text.
//!
//! **It writes nothing to the agent.** The proposal is checked against the
//! draft ([`review`]) and returned; the UI applies what the manager accepts,
//! step by step, through the ordinary draft, grant and test routes, which
//! check it again. So a scenario that talks the model into proposing
//! anything at all can at most produce an offer the manager sees.
//!
//! **A manager's call, not an agent run.** The call runs on a pool the
//! manager may use, under the manager's own spend limits, and is a usage
//! row of the manager's (`UsageSource::Chat`, like the rest of the session
//! UI). It is in no conversation chain; the agent's own chain gets one
//! `assist_suggested` event per call with who asked, the scenario (the
//! manager's own words, kept for the audit), the pool, model, token counts
//! and which steps were offered or dropped.

use std::time::Duration;

use aiplane_agents::db::agent_audit::{AuditKind, NewEvent};
use aiplane_agents::rates::{self, Counter, Rate, RateExceeded, RateScope, Window};
use aiplane_core::server::db::DbError;
use aiplane_core::server::db::usage::UsageSource;
use aiplane_core::server::db::users::User;
use aiplane_core::server::limits::LimitExceeded;
use aiplane_core::server::principal::PrincipalKind;
use aiplane_core::server::upstreams::{PoolAccess, PoolKind};
use jiff::{SignedDuration, Timestamp};
use serde::Serialize;
use serde_json::{Value, json};

use super::audit::{self, LogError};
use super::pool_choice::{JsonExchange, JsonQuestion, ask_json};
use super::profile::pool_model;
use crate::rama_server::state::RamaState;

mod proposal;
pub mod review;

pub use proposal::changes_schema;
pub use review::{Applied, ReviewContext, Suggestion, apply_changes};

/// The longest scenario the assistant reads.
pub const MAX_SCENARIO_CHARS: usize = 8_000;
/// The longest text "improve" reads.
pub const MAX_IMPROVE_CHARS: usize = 8_000;
pub const MAX_TEMPLATE_CHARS: usize = 200;
/// Calls per manager and agent, suggest and improve together.
pub const ASSIST_RATE: Rate = Rate {
    max: 30,
    per: SignedDuration::from_secs(60 * 60),
};
pub const MAX_TESTS: usize = 6;
const SUGGEST_TIMEOUT: Duration = Duration::from_secs(120);
const IMPROVE_TIMEOUT: Duration = Duration::from_secs(60);

/// The friendly kinds of information a slot step offers.
pub const SLOT_TYPES: &[&str] = &[
    "text",
    "long_text",
    "email",
    "number",
    "whole_number",
    "yes_no",
    "choice",
];
/// The identity checks the identity step knows.
pub const IDENTITY_METHODS: &[&str] = &["none", "website_login", "email_code", "lookup"];
/// The hand-off target that is a person rather than an agent.
pub const HUMAN_TARGET: &str = "human";

/// A tool this manager may grant.
#[derive(Debug, Clone)]
pub struct Ability {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

/// An agent this manager may hand conversations to.
#[derive(Debug, Clone)]
pub struct Target {
    pub id: String,
    pub name: String,
}

/// What the model may choose among, resolved for the calling manager.
#[derive(Debug, Clone, Default)]
pub struct Candidates {
    pub abilities: Vec<Ability>,
    pub agents: Vec<Target>,
    /// The chat pools this manager may use and grant, for an architect's
    /// model choice. The suggestion's schema does not offer a pool.
    pub pools: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImproveField {
    Task,
    Tone,
    Refusal,
}

impl ImproveField {
    fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Tone => "tone",
            Self::Refusal => "refusal",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AssistError {
    #[error("{0}")]
    Input(String),
    #[error(
        "pool `{0}` is not a chat pool you may use — leave `pool` out to let the assistant pick \
         one, or name one listed under `pools` in GET /api/v0/agent-resources"
    )]
    PoolNotAllowed(String),
    #[error(
        "none of the chat pools you may use serves a model right now, so the assistant cannot \
         run — try again shortly, or ask an admin which pools your groups may use"
    )]
    NoModel,
    #[error(
        "you asked the assistant {} times within {} for this agent — wait {} seconds and try \
         again",
        .0.max, rate_window(.0.per), .0.retry_after_secs
    )]
    RateLimited(RateExceeded),
    #[error(
        "you are over your usage limit, so the assistant cannot call a model for you — see \
         /usage for your limits; it frees up in {} seconds",
        .0.retry_after_secs
    )]
    OverBudget(LimitExceeded),
    #[error("the assistant's model call failed: {0} — try again, or pick another `pool`")]
    Model(String),
    #[error(transparent)]
    Log(#[from] LogError),
    #[error("the assistant could not reach the database: {0}")]
    Db(#[from] DbError),
}

impl AssistError {
    /// Seconds to wait before asking again, for a `Retry-After`.
    pub fn retry_after_secs(&self) -> Option<i64> {
        match self {
            Self::RateLimited(r) => Some(r.retry_after_secs),
            Self::OverBudget(l) => Some(l.retry_after_secs),
            _ => None,
        }
    }
}

fn rate_window(per: SignedDuration) -> String {
    match per.as_secs() {
        s if s % 3600 == 0 => format!("{} h", s / 3600),
        s if s % 60 == 0 => format!("{} min", s / 60),
        s => format!("{s} s"),
    }
}

/// A suggestion for agent `agent_id`, as the endpoint returns it.
#[derive(Debug, Clone, Serialize)]
pub struct Suggested {
    #[serde(flatten)]
    pub suggestion: Suggestion,
    pub pool: String,
    pub model: String,
    pub usage: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImprovedText {
    pub field: ImproveField,
    pub suggestion: String,
    pub why: String,
    pub pool: String,
    pub model: String,
    pub usage: Value,
}

/// What `POST …/assist/suggest` asks.
pub struct SuggestRequest<'a> {
    pub scenario: &'a str,
    pub template: Option<&'a str>,
    /// The draft the manager is editing: theirs, or the stored one.
    pub base: &'a Value,
    pub pool: Option<&'a str>,
}

/// The manager asking, about which agent.
pub struct Asker<'a> {
    pub state: &'a RamaState,
    pub user: &'a User,
    pub agent_id: &'a str,
}

fn require_text(text: &str, what: &str, max: usize) -> Result<(), AssistError> {
    if text.trim().is_empty() {
        return Err(AssistError::Input(format!(
            "the {what} is empty — describe what the agent should do"
        )));
    }
    let chars = text.chars().count();
    if chars > max {
        return Err(AssistError::Input(format!(
            "the {what} is {chars} characters long; the assistant reads at most {max} — shorten \
             it to what the agent needs to know"
        )));
    }
    Ok(())
}

impl Asker<'_> {
    /// Propose a setup for `request.scenario`, checked against
    /// `request.base` in `ctx`.
    pub async fn suggest(
        &self,
        request: SuggestRequest<'_>,
        ctx: &ReviewContext<'_>,
    ) -> Result<Suggested, AssistError> {
        require_text(request.scenario, "scenario", MAX_SCENARIO_CHARS)?;
        if let Some(t) = request.template
            && t.chars().count() > MAX_TEMPLATE_CHARS
        {
            return Err(AssistError::Input(format!(
                "the template name is longer than {MAX_TEMPLATE_CHARS} characters — pass the \
                 template's id"
            )));
        }
        let draft_pool = request.base.pointer("/main/pool").and_then(Value::as_str);
        let (pool, access) = self.admit(request.pool, draft_pool).await?;
        let input = proposal::suggest_input(
            request.scenario,
            request.template,
            request.base,
            ctx.candidates,
        );
        let exchange = ask_json(
            self.state,
            &pool,
            &access,
            &JsonQuestion {
                instructions: proposal::SUGGEST_INSTRUCTIONS,
                input: &input,
                name: "agent_setup",
                schema: proposal::suggest_schema(ctx.candidates),
                temperature: 0.3,
                timeout: SUGGEST_TIMEOUT,
            },
        )
        .await;
        self.meter(&exchange);
        let suggestion = exchange
            .answer
            .as_ref()
            .ok()
            .map(|answer| review::review(answer, request.base, ctx));
        let mut detail = json!({
            "action": "suggest",
            "scenario": request.scenario,
            "template": request.template,
        });
        if let Some(s) = &suggestion {
            detail["offered"] = json!(s.steps.offered());
            detail["dropped"] = json!(s.dropped);
        }
        self.record(&pool, &exchange, detail).await?;
        let suggestion = suggestion.ok_or_else(|| model_error(&exchange))?;
        Ok(Suggested {
            suggestion,
            pool,
            model: exchange.model.clone().unwrap_or_default(),
            usage: usage_of(&exchange),
        })
    }

    /// A better version of `text` for `field`.
    pub async fn improve(
        &self,
        field: ImproveField,
        text: &str,
        requested_pool: Option<&str>,
        draft: &Value,
    ) -> Result<ImprovedText, AssistError> {
        require_text(text, "text", MAX_IMPROVE_CHARS)?;
        let draft_pool = draft.pointer("/main/pool").and_then(Value::as_str);
        let (pool, access) = self.admit(requested_pool, draft_pool).await?;
        let input = json!({ "field": proposal::improve_purpose(field), "text": text }).to_string();
        let exchange = ask_json(
            self.state,
            &pool,
            &access,
            &JsonQuestion {
                instructions: proposal::IMPROVE_INSTRUCTIONS,
                input: &input,
                name: "improved_text",
                schema: proposal::improve_schema(),
                temperature: 0.3,
                timeout: IMPROVE_TIMEOUT,
            },
        )
        .await;
        self.meter(&exchange);
        let improved = exchange
            .answer
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|a| {
                serde_json::from_value::<proposal::Improved>(a.clone())
                    .map_err(|e| format!("the answer does not read as an improved text ({e})"))
                    .and_then(|i| {
                        if i.suggestion.trim().is_empty() {
                            Err("the model returned an empty text".to_string())
                        } else {
                            Ok(i)
                        }
                    })
            });
        self.record(
            &pool,
            &exchange,
            json!({
                "action": "improve",
                "field": field.as_str(),
                "text": text,
                "error": improved.as_ref().err(),
            }),
        )
        .await?;
        let improved = improved.map_err(AssistError::Model)?;
        Ok(ImprovedText {
            field,
            suggestion: improved.suggestion.trim().to_string(),
            why: improved.why.trim().to_string(),
            pool,
            model: exchange.model.clone().unwrap_or_default(),
            usage: usage_of(&exchange),
        })
    }

    /// The pool to ask, once the manager's rate and spend limits allow a
    /// call: `requested` if they may use it, else the draft's `main.pool`
    /// if they may, else the admin's "Balanced" model choice
    /// (`agents.pool_balanced`, #116) if they may, else the first chat pool
    /// they may use that serves a model.
    async fn admit(
        &self,
        requested: Option<&str>,
        draft_pool: Option<&str>,
    ) -> Result<(String, PoolAccess), AssistError> {
        let access = self.state.pool_access_for(&self.user.roles);
        let (pool, model) = choose_pool(self.state, &access, requested, draft_pool)?;
        let window = Window {
            scope: RateScope::Manager,
            rate: ASSIST_RATE,
            counter: Counter::new(format!("assist:{}", self.user.id)),
        };
        rates::record_now(&self.state.db, self.agent_id, &[window], Timestamp::now())
            .await?
            .map_err(AssistError::RateLimited)?;
        let role_ids = self.state.role_ids_for(&self.user.roles);
        self.state
            .enforcer
            .check_for_model(
                &self.user.id,
                &role_ids,
                &model,
                self.state
                    .upstreams
                    .enforce_limits_for_model(&model, PoolKind::Chat),
            )
            .await
            .map_err(AssistError::OverBudget)?;
        Ok((pool, access))
    }

    fn meter(&self, exchange: &JsonExchange) {
        if !self.state.usage.is_enabled() {
            return;
        }
        if let Some(row) = exchange.usage_record(
            self.state,
            &self.user.id,
            Some(self.user.email.clone()),
            UsageSource::Chat,
            PrincipalKind::User,
        ) {
            self.state.usage.emit(row);
        }
    }

    async fn record(
        &self,
        pool: &str,
        exchange: &JsonExchange,
        mut detail: Value,
    ) -> Result<(), AssistError> {
        detail["pool"] = json!(pool);
        detail["model"] = json!(exchange.model);
        detail["usage"] = usage_of(exchange);
        detail["latency_ms"] = json!(exchange.latency_ms);
        if let Err(error) = &exchange.answer {
            detail["error"] = json!(error);
        }
        audit::record_event(
            &self.state.db,
            NewEvent::new(AuditKind::AssistSuggested, self.agent_id, detail)
                .by(Some(&self.user.id)),
        )
        .await?;
        Ok(())
    }
}

fn model_error(exchange: &JsonExchange) -> AssistError {
    AssistError::Model(
        exchange
            .answer
            .as_ref()
            .err()
            .cloned()
            .unwrap_or_else(|| "no answer".into()),
    )
}

fn usage_of(exchange: &JsonExchange) -> Value {
    let u = exchange.response.get("usage");
    json!({
        "prompt_tokens": u.and_then(|u| u.get("prompt_tokens")),
        "completion_tokens": u.and_then(|u| u.get("completion_tokens")),
        "total_tokens": u.and_then(|u| u.get("total_tokens")),
    })
}

/// See [`Asker::admit`]; the pool and the model it would serve.
pub fn choose_pool(
    state: &RamaState,
    access: &PoolAccess,
    requested: Option<&str>,
    draft_pool: Option<&str>,
) -> Result<(String, String), AssistError> {
    let with_model = |pool: &str| pool_model(state, pool, access).map(|m| (pool.to_string(), m));
    if let Some(pool) = requested {
        let usable = state
            .upstreams
            .pools()
            .into_iter()
            .any(|p| p.name == pool && p.kind == PoolKind::Chat && access.allows(&p));
        if !usable {
            return Err(AssistError::PoolNotAllowed(pool.to_string()));
        }
        return with_model(pool).ok_or(AssistError::NoModel);
    }
    if let Some(found) = draft_pool.and_then(with_model) {
        return Ok(found);
    }
    let balanced = state.config().agents.pool_balanced.clone();
    if let Some(found) = balanced.as_deref().and_then(with_model) {
        return Ok(found);
    }
    let mut pools: Vec<String> = state
        .upstreams
        .pools()
        .into_iter()
        .filter(|p| p.kind == PoolKind::Chat && access.allows(p))
        .map(|p| p.name.clone())
        .collect();
    pools.sort();
    pools
        .iter()
        .find_map(|p| with_model(p))
        .ok_or(AssistError::NoModel)
}
