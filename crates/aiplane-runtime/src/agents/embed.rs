// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! What the public agent endpoint (`/api/v0/embed/*`, `docs/agents.md` §5)
//! needs from the runtime: the visitor-session settings read from an agent's
//! live spec, and the seam through which it runs one turn.
//!
//! The endpoint owns everything about the visitor — key, origin, token, TTL,
//! which conversation — and opens the turn rows. Running the turn as the
//! agent's principal is [`AgentTurnRunner`]'s job, so the endpoint never
//! touches the loop, grants or gates.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use aiplane_core::server::db::agent_audit::{self, AuditKind};
use aiplane_core::server::db::agents as agents_db;
use aiplane_core::server::db::limits::{Dimension, EffectiveLimit, SubjectType, Window};
use aiplane_core::server::limits::{LimitExceeded, Rate, RateExceeded, VisitorKey, VisitorRates};
use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};

use session_core::db as chat;

use session_core::i18n::Lang;

use super::profile::{Role, RunOptions, RunProfile};
pub use super::resume::ClaimedResume;
use super::resume::run_claimed;
pub use super::run::OpenedTurn;
use super::run::drive_opened;
use crate::rama_server::state::RamaState;

/// A visitor session's idle TTL when the spec names no `publish.idle_ttl`.
pub const DEFAULT_IDLE_TTL: SignedDuration = SignedDuration::from_secs(30 * 60);

/// The absolute cap on one visitor session, however often it is used:
/// `sessionStorage` already ends it with the tab, and this ends a tab that
/// is never closed.
pub const MAX_VISITOR_SESSION: SignedDuration = SignedDuration::from_secs(24 * 60 * 60);

/// `publish.idle_ttl` of a spec, or [`DEFAULT_IDLE_TTL`]. A published spec
/// passed validation, so an unparsable value only comes from a hand-edited
/// row; it falls back rather than locking every visitor out.
pub fn idle_ttl(spec: &Value) -> SignedDuration {
    spec.pointer("/publish/idle_ttl")
        .and_then(Value::as_str)
        .and_then(super::spec::parse_duration)
        .unwrap_or(DEFAULT_IDLE_TTL)
}

/// Whether the spec lets a browser on `origin` use the agent: always when it
/// sets no `publish.origins`, otherwise only an origin listed there. The
/// embed key's own list applies on top; an origin must pass both.
pub fn spec_allows_origin(spec: &Value, origin: &str) -> bool {
    match spec.pointer("/publish/origins").and_then(Value::as_array) {
        None => true,
        Some(list) => list.iter().any(|o| o.as_str() == Some(origin)),
    }
}

/// Messages one visitor session may send when the spec sets no
/// `publish.rate_limits.visitor`: a person typing, with room to spare.
pub const DEFAULT_VISITOR_RATE: Rate = Rate {
    max: 20,
    per: SignedDuration::from_secs(10 * 60),
};

/// Conversations started plus messages sent from one client IP when the spec
/// sets no `publish.rate_limits.ip`: an office behind one NAT, not a script.
pub const DEFAULT_IP_RATE: Rate = Rate {
    max: 60,
    per: SignedDuration::from_secs(10 * 60),
};

fn rate(spec: &Value, scope: &str, default: Rate) -> Rate {
    let at = |key: &str| spec.pointer(&format!("/publish/rate_limits/{scope}/{key}"));
    let max = at("max")
        .and_then(Value::as_u64)
        .and_then(|m| u32::try_from(m).ok())
        .filter(|m| *m > 0);
    let per = at("per")
        .and_then(Value::as_str)
        .and_then(super::spec::parse_duration);
    match (max, per) {
        (Some(max), Some(per)) => Rate { max, per },
        _ => default,
    }
}

/// `publish.rate_limits` of a spec, each scope falling back to its default.
pub fn visitor_rates(spec: &Value) -> VisitorRates {
    VisitorRates {
        visitor: rate(spec, "visitor", DEFAULT_VISITOR_RATE),
        ip: rate(spec, "ip", DEFAULT_IP_RATE),
    }
}

/// `publish.budget` of a spec as limits over the month: what the owner lets
/// the agent's conversations spend. None by default: an owner who sets no
/// budget relies on the operator's limits on the agent and its pools.
pub fn owner_budget(spec: &Value) -> Vec<EffectiveLimit> {
    let monthly = |dimension, value: Option<f64>| {
        value.filter(|v| *v > 0.0).map(|value| EffectiveLimit {
            model: None,
            dimension,
            window: Window::Month,
            value,
            source: SubjectType::AgentSpec,
        })
    };
    let budget = |key: &str| spec.pointer(&format!("/publish/budget/{key}"));
    [
        monthly(
            Dimension::Cost,
            budget("monthly_cost").and_then(Value::as_f64),
        ),
        monthly(
            Dimension::Tokens,
            budget("monthly_tokens")
                .and_then(Value::as_u64)
                .map(|t| t as f64),
        ),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Who is asking: the visitor session once there is one, and the client IP
/// when the request carries one.
#[derive(Debug, Clone, Copy)]
pub struct Admission<'a> {
    pub visitor_id: Option<&'a str>,
    /// The A2A context asking, which counts like a visitor session.
    pub a2a_context: Option<&'a str>,
    pub ip: Option<&'a str>,
}

/// Why a visitor request was refused before it reached the agent.
#[derive(Debug, Clone)]
pub enum Refusal {
    /// Too many requests from this visitor or IP; wait `retry_after_secs`.
    Rate(RateExceeded),
    /// The agent's conversations have spent a budget; it is unavailable
    /// until usage leaves the window.
    Budget(LimitExceeded),
}

impl Refusal {
    pub fn retry_after_secs(&self) -> i64 {
        match self {
            Refusal::Rate(r) => r.retry_after_secs,
            Refusal::Budget(b) => b.retry_after_secs,
        }
    }

    /// What the agent's managers read in its audit trail and in the agents
    /// API: which limit, and the numbers behind it.
    pub fn detail(&self, visitor_id: Option<&str>) -> Value {
        match self {
            Refusal::Rate(r) => json!({
                "limit": format!("{}_rate", r.scope.as_str()),
                "visitor_id": visitor_id,
                "max": r.max,
                "per_secs": r.per.as_secs(),
                "retry_after_secs": r.retry_after_secs,
            }),
            Refusal::Budget(b) => budget_detail(b),
        }
    }
}

/// The budget breach as the owner reads it.
pub fn budget_detail(b: &LimitExceeded) -> Value {
    json!({
        "limit": "budget",
        "set_by": if b.subject == SubjectType::AgentSpec { "agent" } else { "operator" },
        "dimension": b.dimension.as_str(),
        "window": b.window.as_str(),
        "max": b.limit,
        "used": b.used,
        "retry_after_secs": b.retry_after_secs,
    })
}

/// Gate one visitor request to `agent_id` on the agent's live
/// `publish.rate_limits` and budget, at `now` (a parameter so tests can
/// stand inside a window). Every refusal is audited on the agent. An agent
/// with no live version is not gated here; the endpoint refuses it anyway.
pub async fn admit(
    state: &RamaState,
    agent_id: &str,
    who: Admission<'_>,
    now: Timestamp,
) -> Result<(), Refusal> {
    let spec = match agents_db::live(&state.db, agent_id).await {
        Ok(Some((_, text))) => serde_json::from_str(&text).unwrap_or(Value::Null),
        Ok(None) => return Ok(()),
        Err(err) => {
            tracing::warn!(error = %err, agent = agent_id, "reading the agent's limits; admitting");
            return Ok(());
        }
    };
    let key = VisitorKey {
        principal_id: agent_id,
        visitor_id: who.visitor_id,
        a2a_context: who.a2a_context,
        ip: who.ip,
    };
    let refused = match state
        .enforcer
        .check_visitor(&visitor_rates(&spec), &key, now)
        .await
    {
        Err(rate) => Refusal::Rate(rate),
        Ok(()) => match state
            .enforcer
            .check_agent(agent_id, &owner_budget(&spec), now)
            .await
        {
            Err(budget) => Refusal::Budget(budget),
            Ok(()) => return Ok(()),
        },
    };
    if let Err(err) = agent_audit::record_run_event(
        &state.db,
        AuditKind::LimitRefused,
        agent_id,
        None,
        refused.detail(who.visitor_id),
    )
    .await
    {
        tracing::warn!(error = %err, agent = agent_id, "recording a refused visitor request");
    }
    Err(refused)
}

/// What an agent's managers see of its limits under the live spec `live`
/// (`None` before it is published): the visitor rates and retention in
/// force, each budget with what has been spent against it, and whether the
/// agent is turning visitors away right now and why.
pub async fn limits_view(
    state: &RamaState,
    agent_id: &str,
    live: Option<&Value>,
    now: Timestamp,
) -> Value {
    let spec = live.cloned().unwrap_or(Value::Null);
    let rates = visitor_rates(&spec);
    let rate = |r: Rate| json!({ "max": r.max, "per_secs": r.per.as_secs() });
    let statuses = state
        .enforcer
        .agent_statuses(agent_id, &owner_budget(&spec), now)
        .await;
    let budget: Vec<Value> = statuses
        .iter()
        .map(|s| {
            json!({
                "set_by": if s.source == SubjectType::AgentSpec { "agent" } else { "operator" },
                "dimension": s.dimension.as_str(),
                "window": s.window.as_str(),
                "max": s.limit,
                "used": s.used,
                "exceeded": s.exceeded(),
                "refreshes_at": s.refreshes_at,
            })
        })
        .collect();
    let exhausted = state
        .enforcer
        .check_agent(agent_id, &owner_budget(&spec), now)
        .await
        .err();
    json!({
        "rate_limits": { "visitor": rate(rates.visitor), "ip": rate(rates.ip) },
        "retention_days": super::retention::retention_days(&spec),
        "budget": budget,
        "available": exhausted.is_none(),
        "unavailable_reason": exhausted.as_ref().map(budget_detail),
    })
}

/// Runs one opened turn of an agent conversation as the agent's principal.
///
/// Contract:
/// - The turn runs as `agent_id` with exactly its grants, on `version` (the
///   version the conversation started on), never as a person.
/// - Output is buffered (`OutputPolicy::Buffered`): the endpoint shows a
///   visitor an assistant answer only once its turn is terminal, so a runner
///   may write partial content as it likes.
/// - When `run` returns, `turn_id` is terminal or suspended (a tool paused
///   it for a decision; see [`crate::agents::resume`]). The endpoint errors a
///   turn left `in_progress`, so a runner that crashes cannot wedge the
///   conversation.
#[async_trait::async_trait]
pub trait AgentTurnRunner: Send + Sync {
    async fn run(&self, state: Arc<RamaState>, turn: OpenedTurn);

    /// Continue a suspended conversation whose decision won the claim. Same
    /// contract as [`Self::run`]: when this returns, the conversation's turn
    /// is terminal or suspended again, never `in_progress`.
    async fn resume(&self, state: Arc<RamaState>, claimed: ClaimedResume, lang: Lang) {
        let turn = claimed.turn_id().to_string();
        if let Err(err) = run_claimed(&state, claimed, RunOptions::default(), lang).await {
            tracing::warn!(error = %err, %turn, "a suspended visitor turn could not resume");
        }
    }
}

/// The production runner: the agent entry point of `agents::run`.
#[derive(Debug, Clone, Copy, Default)]
pub struct LiveAgentRunner;

#[async_trait::async_trait]
impl AgentTurnRunner for LiveAgentRunner {
    async fn run(&self, state: Arc<RamaState>, turn: OpenedTurn) {
        let options = RunOptions {
            lang: turn.lang,
            ..RunOptions::default()
        };
        let ran = match RunProfile::load_version(
            &state,
            &turn.agent_id,
            Some(turn.version),
            Role::Main,
            &options,
        )
        .await
        {
            Ok(profile) => drive_opened(&state, &profile, &turn).await.map(|_| ()),
            Err(err) => Err(err),
        };
        if let Err(err) = ran {
            tracing::warn!(error = %err, turn = %turn.turn_id, "visitor turn could not run");
            if let Err(db) = chat::finalize_turn(
                &state.db,
                &turn.turn_id,
                chat::TurnStatus::Errored,
                Some(&err.to_string()),
            )
            .await
            {
                tracing::warn!(error = %db, turn = %turn.turn_id, "recording a failed visitor turn");
            }
        }
    }
}

/// The runner, if this build has one, and the conversations with a turn
/// running right now, each with the flag that stops it.
#[derive(Clone, Default)]
pub struct AgentTurns {
    runner: Option<Arc<dyn AgentTurnRunner>>,
    running: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
}

impl AgentTurns {
    pub fn new(runner: Arc<dyn AgentTurnRunner>) -> Self {
        Self {
            runner: Some(runner),
            running: Arc::default(),
        }
    }

    pub fn runner(&self) -> Option<Arc<dyn AgentTurnRunner>> {
        self.runner.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<AtomicBool>>> {
        self.running.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Whether a turn of `session_id` is still being produced. Its row can
    /// already be terminal while the output filter (#89) has yet to rule on
    /// the answer, so the public endpoint treats it as unfinished.
    pub fn is_running(&self, session_id: &str) -> bool {
        self.lock().contains_key(session_id)
    }

    /// Claim `session_id` for one turn. `None` when a turn already holds it;
    /// the claim ends when the returned guard drops.
    pub fn claim(&self, session_id: &str) -> Option<TurnClaim> {
        let mut running = self.lock();
        if running.contains_key(session_id) {
            return None;
        }
        running.insert(session_id.to_string(), Arc::default());
        Some(TurnClaim {
            running: self.running.clone(),
            session_id: session_id.to_string(),
        })
    }

    /// The stop flag of the turn holding `session_id`, which the driver of
    /// that conversation's runs reads between rounds (`headless::drive`).
    pub fn cancel_flag(&self, session_id: &str) -> Option<Arc<AtomicBool>> {
        self.lock().get(session_id).cloned()
    }

    /// Ask the turn holding `session_id` to stop at its next check; it then
    /// ends `cancelled`. False when no turn holds the conversation.
    pub fn cancel(&self, session_id: &str) -> bool {
        match self.lock().get(session_id) {
            Some(flag) => {
                flag.store(true, Ordering::SeqCst);
                true
            }
            None => false,
        }
    }
}

/// See [`AgentTurns::claim`].
pub struct TurnClaim {
    running: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    session_id: String,
}

impl Drop for TurnClaim {
    fn drop(&mut self) {
        let mut running = self.running.lock().unwrap_or_else(|p| p.into_inner());
        running.remove(&self.session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_idle_ttl_comes_from_the_publish_settings_with_a_30_minute_default() {
        assert_eq!(
            idle_ttl(&json!({ "publish": { "idle_ttl": "45m" } })),
            SignedDuration::from_secs(45 * 60)
        );
        assert_eq!(idle_ttl(&json!({ "publish": {} })), DEFAULT_IDLE_TTL);
        assert_eq!(idle_ttl(&json!({})), DEFAULT_IDLE_TTL);
        assert_eq!(
            idle_ttl(&json!({ "publish": { "idle_ttl": "soon" } })),
            DEFAULT_IDLE_TTL
        );
        assert_eq!(DEFAULT_IDLE_TTL, SignedDuration::from_secs(1800));
    }

    #[test]
    fn the_specs_origins_narrow_only_when_it_lists_some() {
        let open = json!({ "publish": {} });
        assert!(spec_allows_origin(&open, "https://any.example"));
        let listed = json!({ "publish": { "origins": ["https://a.example"] } });
        assert!(spec_allows_origin(&listed, "https://a.example"));
        assert!(!spec_allows_origin(&listed, "https://b.example"));
        let empty = json!({ "publish": { "origins": [] } });
        assert!(!spec_allows_origin(&empty, "https://a.example"));
    }

    #[test]
    fn visitor_rates_come_from_the_publish_settings_scope_by_scope() {
        let set = visitor_rates(&json!({ "publish": { "rate_limits": {
            "visitor": { "max": 3, "per": "1m" }
        } } }));
        assert_eq!(
            set.visitor,
            Rate {
                max: 3,
                per: SignedDuration::from_secs(60)
            }
        );
        assert_eq!(set.ip, DEFAULT_IP_RATE, "an unset scope keeps its default");
        let none = visitor_rates(&json!({}));
        assert_eq!(none.visitor, DEFAULT_VISITOR_RATE);
        assert_eq!(DEFAULT_VISITOR_RATE.max, 20);
        assert_eq!(DEFAULT_IP_RATE.max, 60);
        let half = visitor_rates(&json!({ "publish": { "rate_limits": {
            "visitor": { "max": 3 }
        } } }));
        assert_eq!(
            half.visitor, DEFAULT_VISITOR_RATE,
            "a rate is both or neither"
        );
    }

    #[test]
    fn the_owner_budget_is_a_monthly_ceiling_per_dimension_and_none_by_default() {
        assert!(owner_budget(&json!({ "publish": {} })).is_empty());
        let both = owner_budget(&json!({ "publish": { "budget": {
            "monthly_cost": 25.5, "monthly_tokens": 1000
        } } }));
        let cells: Vec<(Dimension, Window, f64, SubjectType)> = both
            .iter()
            .map(|l| (l.dimension, l.window, l.value, l.source))
            .collect();
        assert_eq!(
            cells,
            [
                (Dimension::Cost, Window::Month, 25.5, SubjectType::AgentSpec),
                (
                    Dimension::Tokens,
                    Window::Month,
                    1000.0,
                    SubjectType::AgentSpec
                ),
            ]
        );
    }

    #[test]
    fn a_conversation_holds_one_claim_at_a_time() {
        let turns = AgentTurns::default();
        assert!(turns.runner().is_none());
        let first = turns.claim("s1").expect("free");
        assert!(turns.claim("s1").is_none(), "already running");
        assert!(turns.claim("s2").is_some(), "other conversations are free");
        drop(first);
        assert!(turns.claim("s1").is_some(), "released on drop");
    }

    #[test]
    fn a_claimed_turn_can_be_asked_to_stop_and_an_unclaimed_one_cannot() {
        let turns = AgentTurns::default();
        assert!(!turns.cancel("s1"), "nothing runs");
        let claim = turns.claim("s1").expect("free");
        let flag = turns.cancel_flag("s1").expect("a running turn has a flag");
        assert!(!flag.load(Ordering::SeqCst));
        assert!(turns.cancel("s1"));
        assert!(flag.load(Ordering::SeqCst), "the driver sees the request");
        drop(claim);
        assert!(turns.cancel_flag("s1").is_none());
        let again = turns.claim("s1").expect("free again");
        assert!(
            !turns.cancel_flag("s1").unwrap().load(Ordering::SeqCst),
            "a new turn starts with a fresh flag"
        );
        drop(again);
    }
}
