// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The shared "run a prompt headlessly" helper.
//!
//! Both cron-fired scheduled actions ([`crate::server::scheduled::worker`])
//! and inbound-webhook fires (`rama_server::pages::webhooks`) need the same
//! thing: open a chat session, append the prompt + an in-progress assistant
//! turn, then drive it to completion through the same [`OpenAiDriver`] the
//! interactive `/chat` path uses — so the result lands as an ordinary
//! conversation the owner can open afterwards.
//!
//! The work is split in two so callers can act between the steps:
//!   - [`open_session`] mints (or reuses) the session and appends the prompt +
//!     an in-progress assistant turn, returning `(session_id,
//!     assistant_turn_id)`. A caller that must respond *before* the model
//!     finishes (an async webhook returning `202` with the session id) needs
//!     these ids up front.
//!   - [`drive`] builds the driver and runs the turn to completion.
//!
//! After [`drive`] returns, read the finished assistant turn
//! (`session_core::db::get_turn`) to classify the outcome or return its text.
//! A run given a [`FinishContract`] instead gets its structured
//! [`RunOutcome`] back from [`drive`] (see [`crate::finish`]).
//!
//! [`OpenAiDriver`]: crate::openai_driver::OpenAiDriver

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use session_core::db as chat;
use uuid::Uuid;

use crate::budget::{Budget, Clock};
use crate::finish::{FinishContract, FinishRun, IncompleteReason, RunOutcome};
use crate::rama_server::state::RamaState;
use aiplane_core::server::db::usage::UsageSource;
use aiplane_core::server::db::{DbError, Pool};

/// Inputs to [`open_session`].
pub struct OpenParams<'a> {
    pub user_id: &'a str,
    /// Title for a freshly-minted session (ignored when `existing_session` is
    /// `Some`).
    pub title: &'a str,
    pub prompt: &'a str,
    pub model: &'a str,
    /// When `Some`, append into this existing session instead of minting a
    /// fresh one — so prior runs become conversation history. `None` mints a
    /// fresh session titled `title`.
    pub existing_session: Option<String>,
}

/// Open the run's session and append the prompt + an in-progress assistant
/// turn. Returns `(session_id, assistant_turn_id)`.
///
/// Every call mints fresh turn ids, so repeated runs from the same source
/// never PRIMARY KEY-clash — whether they share a session (reuse) or not.
pub async fn open_session(db: &Pool, p: OpenParams<'_>) -> Result<(String, String), DbError> {
    let session_id = match p.existing_session {
        Some(id) => id,
        None => {
            let session = chat::create_session(db, p.user_id).await?;
            chat::set_session_title(db, &session.id, p.title).await?;
            session.id
        }
    };

    let user_turn_id = Uuid::new_v4().to_string();
    chat::create_user_turn(db, &session_id, &user_turn_id, p.prompt).await?;

    let assistant_turn_id = Uuid::new_v4().to_string();
    chat::create_assistant_turn_in_progress(db, &session_id, &assistant_turn_id, p.model).await?;
    Ok((session_id, assistant_turn_id))
}

/// Inputs to [`drive`]. Tools are gated by `roles`: pass the owner's roles to
/// offer their normal tools, or an empty vec to offer none.
pub struct DriveParams {
    pub user_id: String,
    pub roles: Vec<String>,
    pub session_id: String,
    pub assistant_turn_id: String,
    pub model: String,
    pub source: UsageSource,
    /// Cap on how many prior turns the driver replays (`None` = no cap, the
    /// fresh-chat default). Callers that reuse a session set this to bound the
    /// replayed history.
    pub history_limit: Option<usize>,
    /// When `Some`, the run ends only through a schema-valid `finish` call or
    /// a structured incomplete outcome, and [`drive`] returns which. `None`
    /// runs the turn exactly as an interactive chat turn would.
    pub finish: Option<FinishContract>,
    /// What the run may spend. `None` takes the round cap of the session's
    /// effort level and no time or token limit.
    pub budget: Option<Budget>,
}

/// Drive an already-opened turn to completion through the `OpenAiDriver`.
///
/// Returns the run's [`RunOutcome`] when it was given a finish contract, and
/// `None` otherwise.
pub async fn drive(state: &Arc<RamaState>, p: DriveParams) -> Option<RunOutcome> {
    drive_with_clock(state, p, crate::budget::system_clock()).await
}

/// [`drive`] with the run's clock supplied, so a `seconds` budget is testable.
pub async fn drive_with_clock(
    state: &Arc<RamaState>,
    p: DriveParams,
    clock: Clock,
) -> Option<RunOutcome> {
    let finish = p.finish.map(FinishRun::new);
    let session_id = p.session_id.clone();
    let assistant_turn_id = p.assistant_turn_id.clone();
    let tool_ctx = crate::openai_driver::build_tool_context(
        state,
        crate::openai_driver::TurnFacts {
            user_id: p.user_id.clone(),
            roles: p.roles,
            session_id: p.session_id.clone(),
            assistant_turn_id: p.assistant_turn_id.clone(),
            // Headless: no request, so no client IP, and nobody watching the
            // stream to answer an interactive prompt.
            client_ip: None,
            chat_feedback: None,
            model: Some(p.model.clone()),
            // Session path: access is exactly the user's group grant.
            pool_access: None,
        },
    );
    let driver = Box::new(crate::openai_driver::OpenAiDriver {
        state: state.clone(),
        tool_ctx,
        source: p.source,
        history_limit: p.history_limit,
        voice_mode: false,
        finish: finish.clone(),
        budget: p.budget,
        clock,
    });

    // No registry slot and a throwaway broadcast channel: a headless run has no
    // live viewer to tail or cancel it. The DB is the source of truth, so
    // dropping every frame is fine.
    let (broadcast, _rx) = tokio::sync::broadcast::channel(16);
    let ctx = session_core::driver::SessionContext {
        user_id: Some(p.user_id),
        session_id: p.session_id,
        assistant_turn_id: p.assistant_turn_id,
        model: p.model,
        cancel: Arc::new(AtomicBool::new(false)),
        broadcast,
        // Nobody can interject into a scheduled run: there is no composer
        // attached to it. An always-empty inbox is the honest expression of
        // that, and costs the driver one lock-free check per round.
        steers: session_core::workers::SteerInbox::default(),
    };
    session_core::worker::run_session_turn(state.db.clone(), driver, ctx).await;

    let run = finish?;
    Some(match run.take() {
        Some(outcome) => outcome,
        None => unsettled_outcome(&state.db, &session_id, &assistant_turn_id).await,
    })
}

/// The outcome of a contracted run the driver never settled: it errored,
/// was cancelled, or crashed on the way, and only the turn row knows which.
async fn unsettled_outcome(db: &Pool, session_id: &str, turn_id: &str) -> RunOutcome {
    let turn = chat::get_turn(db, session_id, turn_id).await;
    let reason = match turn {
        Ok(Some(t)) if t.status == chat::TurnStatus::Cancelled => IncompleteReason::Cancelled,
        Ok(Some(t)) => IncompleteReason::Failed {
            message: t
                .error_message
                .unwrap_or_else(|| "the run ended without a finish call".into()),
        },
        Ok(None) => IncompleteReason::Failed {
            message: format!("the run's turn `{turn_id}` no longer exists"),
        },
        Err(e) => IncompleteReason::Failed {
            message: format!("reading the run's turn `{turn_id}` after it ended: {e}"),
        },
    };
    RunOutcome::Incomplete {
        reason,
        summary: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::{Value, json};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::finish::{FINISH_TOOL_NAME, FinishContract, IncompleteReason, RunOutcome};
    use aiplane_core::server::upstreams::{
        self,
        config::{BackendConfig, PickerStrategy, PoolKind, UpstreamPoolConfig},
    };

    const MODEL: &str = "model-a";

    /// A chat upstream that answers each round with the next scripted delta,
    /// repeating the last one once the script runs out.
    struct Scripted {
        deltas: Vec<Value>,
        served: AtomicUsize,
        /// Total tokens the upstream reports for every round, when set.
        tokens_per_round: Option<u64>,
    }

    impl wiremock::Respond for Scripted {
        fn respond(&self, _req: &wiremock::Request) -> ResponseTemplate {
            let i = self.served.fetch_add(1, Ordering::SeqCst);
            let delta = &self.deltas[i.min(self.deltas.len() - 1)];
            let usage = self.tokens_per_round.map(|t| {
                format!(
                    "data: {}\n\n",
                    json!({"choices": [], "usage": {"prompt_tokens": t / 2,
                        "completion_tokens": t - t / 2, "total_tokens": t}})
                )
            });
            let sse = format!(
                "data: {}\n\n{}data: [DONE]\n\n",
                json!({"choices": [{"index": 0, "delta": delta}]}),
                usage.unwrap_or_default()
            );
            ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream")
        }
    }

    fn text(s: &str) -> Value {
        json!({"content": s})
    }

    fn finish_call(id: &str, arguments: Value) -> Value {
        json!({"tool_calls": [{"index": 0, "id": id, "type": "function",
            "function": {"name": FINISH_TOOL_NAME, "arguments": arguments.to_string()}}]})
    }

    fn contract() -> FinishContract {
        FinishContract::new(json!({
            "type": "object",
            "properties": {"status": {"type": "string", "enum": ["resolved", "escalated"]}},
            "required": ["status"],
        }))
        .unwrap()
    }

    async fn state_for(upstream: &str) -> Arc<RamaState> {
        let db = aiplane_core::server::db::open(std::path::Path::new(":memory:"))
            .await
            .unwrap();
        let mut pools = HashMap::new();
        pools.insert(
            "pool".to_string(),
            UpstreamPoolConfig {
                voices: Default::default(),
                offer_voices: Vec::new(),
                allowed_groups: Vec::new(),
                fallback_offline: None,
                compliance: Default::default(),
                enforce_limits: true,
                kind: PoolKind::Chat,
                strategy: PickerStrategy::RoundRobin,
                models: Vec::new(),
                backend: vec![BackendConfig {
                    alias: None,
                    supports_edit: false,
                    enabled: true,
                    name: "mock".into(),
                    base_url: upstream.into(),
                    api_key_env: None,
                    api_key: None,
                    weight: 1,
                    max_inflight: 16,
                    health_path: "/models".into(),
                    models: Vec::new(),
                }],
            },
        );
        let registry = upstreams::UpstreamRegistry::new(&pools).unwrap();
        registry.pools()[0].backends[0].set_models([MODEL.to_string()].into());
        let config = aiplane_core::server::Config {
            gateway: aiplane_core::server::config::GatewayConfig {
                upstream_wait_secs: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        let app = crate::server::AppState::new(
            config,
            db.clone(),
            registry,
            Arc::new(
                crate::server::tools::ToolRegistry::new().with(crate::server::tools::echo::Echo),
            ),
            Arc::new(aiplane_core::server::rbac::Resolver::empty()),
        );
        let sessions = aiplane_core::rama_server::SessionStore::new(db, [7u8; 32]);
        Arc::new(RamaState::new(
            app,
            sessions,
            aiplane_core::server::usage::UsageHandle::disabled(),
        ))
    }

    struct Run {
        outcome: Option<RunOutcome>,
        requests: Vec<Value>,
        turn: session_core::db::TurnWithTools,
    }

    async fn open(state: &Arc<RamaState>, effort: &str) -> (String, String) {
        let now = jiff::Timestamp::now();
        aiplane_core::server::db::users::upsert(
            &state.db,
            &aiplane_core::server::db::users::User {
                id: "u1".into(),
                email: "u1@example.com".into(),
                name: None,
                roles: vec![],
                created_at: now,
                updated_at: now,
                timezone: None,
                speech_voice: None,
            },
        )
        .await
        .unwrap();
        let (session_id, assistant_turn_id) = open_session(
            &state.db,
            OpenParams {
                user_id: "u1",
                title: "run",
                prompt: "triage the ticket",
                model: MODEL,
                existing_session: None,
            },
        )
        .await
        .unwrap();
        aiplane_core::server::db::chat_session_settings::set_effort(&state.db, &session_id, effort)
            .await
            .unwrap();
        (session_id, assistant_turn_id)
    }

    fn params(session_id: &str, turn_id: &str, finish: Option<FinishContract>) -> DriveParams {
        DriveParams {
            user_id: "u1".into(),
            roles: Vec::new(),
            session_id: session_id.into(),
            assistant_turn_id: turn_id.into(),
            model: MODEL.into(),
            source: UsageSource::Scheduled,
            history_limit: None,
            finish,
            budget: None,
        }
    }

    async fn run(deltas: Vec<Value>, finish: Option<FinishContract>, effort: &str) -> Run {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(Scripted {
                deltas,
                served: AtomicUsize::new(0),
                tokens_per_round: None,
            })
            .mount(&upstream)
            .await;
        let state = state_for(&upstream.uri()).await;
        let (session_id, turn_id) = open(&state, effort).await;
        let outcome = drive(&state, params(&session_id, &turn_id, finish)).await;
        let requests = upstream
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|r| serde_json::from_slice(&r.body).unwrap())
            .collect();
        let turn = chat::list_turns(&state.db, &session_id)
            .await
            .unwrap()
            .into_iter()
            .find(|t| t.turn.id == turn_id)
            .unwrap();
        Run {
            outcome,
            requests,
            turn,
        }
    }

    fn offered_tools(request: &Value) -> Vec<&str> {
        request["tools"]
            .as_array()
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|t| t["function"]["name"].as_str())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn last_message(request: &Value) -> &Value {
        request["messages"].as_array().unwrap().last().unwrap()
    }

    fn finished(result: Value) -> Option<RunOutcome> {
        Some(RunOutcome::Finished { result })
    }

    #[tokio::test]
    async fn a_valid_finish_ends_the_run_with_its_result() {
        let r = run(
            vec![finish_call("c1", json!({"result": {"status": "resolved"}}))],
            Some(contract()),
            "standard",
        )
        .await;
        assert_eq!(r.outcome, finished(json!({"status": "resolved"})));
        assert_eq!(r.requests.len(), 1);
        assert_eq!(offered_tools(&r.requests[0]), [FINISH_TOOL_NAME]);
        let system = r.requests[0]["messages"][0]["content"].as_str().unwrap();
        assert!(system.contains("non-interactive run"), "{system}");
        assert_eq!(r.turn.turn.status, session_core::db::TurnStatus::Completed);
        let call = &r.turn.tool_calls[0];
        assert_eq!(call.name, FINISH_TOOL_NAME);
        assert_eq!(call.status, session_core::db::ToolCallStatus::Completed);
    }

    #[tokio::test]
    async fn an_invalid_finish_goes_back_to_the_model_with_the_reason() {
        let r = run(
            vec![
                finish_call("c1", json!({"result": {"status": "done"}})),
                finish_call("c2", json!({"result": {"status": "escalated"}})),
            ],
            Some(contract()),
            "standard",
        )
        .await;
        assert_eq!(r.outcome, finished(json!({"status": "escalated"})));
        assert_eq!(r.requests.len(), 2);
        let answer = last_message(&r.requests[1]);
        assert_eq!(answer["role"], "tool");
        assert_eq!(answer["tool_call_id"], "c1");
        let reason = answer["content"].as_str().unwrap();
        assert!(
            reason.contains("does not match the required schema"),
            "{reason}"
        );
        assert!(reason.contains("/status"), "{reason}");
        assert_eq!(
            r.turn.tool_calls[0].status,
            session_core::db::ToolCallStatus::Errored
        );
    }

    #[tokio::test]
    async fn text_without_finish_gets_a_nudge() {
        let r = run(
            vec![
                text("The ticket looks resolved."),
                finish_call("c1", json!({"result": {"status": "resolved"}})),
            ],
            Some(contract()),
            "standard",
        )
        .await;
        assert_eq!(r.outcome, finished(json!({"status": "resolved"})));
        assert_eq!(r.requests.len(), 2);
        let messages = r.requests[1]["messages"].as_array().unwrap();
        let replayed = &messages[messages.len() - 2];
        assert_eq!(replayed["role"], "assistant");
        assert_eq!(replayed["content"], "The ticket looks resolved.");
        let nudge = last_message(&r.requests[1]);
        assert_eq!(nudge["role"], "user");
        assert!(
            nudge["content"]
                .as_str()
                .unwrap()
                .contains("did not call `finish`"),
            "{nudge}"
        );
    }

    #[tokio::test]
    async fn an_exhausted_budget_ends_incomplete_with_an_account() {
        let r = run(
            vec![text("Still looking into it.")],
            Some(contract()),
            "fast",
        )
        .await;
        let rounds = aiplane_core::server::reasoning::Effort::Fast.max_rounds();
        assert_eq!(
            r.outcome,
            Some(RunOutcome::Incomplete {
                reason: IncompleteReason::RoundBudgetExhausted { rounds },
                summary: "Still looking into it.".into(),
            })
        );
        assert_eq!(r.requests.len(), rounds as usize, "no closing round");
        let last = r.requests.last().unwrap();
        assert_eq!(offered_tools(last), [FINISH_TOOL_NAME]);
        let system = last["messages"][0]["content"].as_str().unwrap();
        assert!(system.contains("FINAL round for this run"), "{system}");
        assert!(
            r.turn.turn.error_message.is_some(),
            "the turn must say it stopped short"
        );
    }

    #[tokio::test]
    async fn a_valid_finish_on_the_final_round_still_finishes() {
        let rounds = aiplane_core::server::reasoning::Effort::Fast.max_rounds() as usize;
        let mut script = vec![text("Working."); rounds - 1];
        script.push(finish_call(
            "last",
            json!({"result": {"status": "resolved"}}),
        ));
        let r = run(script, Some(contract()), "fast").await;
        assert_eq!(r.outcome, finished(json!({"status": "resolved"})));
        assert_eq!(r.requests.len(), rounds);
        assert_eq!(r.turn.turn.error_message, None);
    }

    #[tokio::test]
    async fn finish_alongside_other_calls_is_refused_until_it_comes_alone() {
        let mixed = json!({"tool_calls": [
            {"index": 0, "id": "c1", "type": "function",
             "function": {"name": "lookup", "arguments": "{}"}},
            {"index": 1, "id": "c2", "type": "function",
             "function": {"name": FINISH_TOOL_NAME,
                          "arguments": json!({"result": {"status": "resolved"}}).to_string()}},
        ]});
        let r = run(
            vec![
                mixed,
                finish_call("c3", json!({"result": {"status": "resolved"}})),
            ],
            Some(contract()),
            "standard",
        )
        .await;
        assert_eq!(r.outcome, finished(json!({"status": "resolved"})));
        let messages = r.requests[1]["messages"].as_array().unwrap();
        let refusal = messages
            .iter()
            .find(|m| m["tool_call_id"] == "c2")
            .expect("the early finish is answered");
        assert!(
            refusal["content"]
                .as_str()
                .unwrap()
                .contains("call it on its own"),
            "{refusal}"
        );
    }

    #[tokio::test]
    async fn a_repeated_call_stop_ends_the_run_incomplete() {
        let echo = json!({"tool_calls": [{"index": 0, "id": "", "type": "function",
            "function": {"name": "company_echo", "arguments": r#"{"message":"hi"}"#}}]});
        let r = run(vec![echo], Some(contract()), "standard").await;
        let Some(RunOutcome::Incomplete {
            reason: IncompleteReason::RepeatedToolCall { tool },
            summary,
        }) = r.outcome
        else {
            panic!("expected a repeated-call outcome, got {:?}", r.outcome);
        };
        assert_eq!(tool, "company_echo");
        assert!(summary.contains("identical"), "{summary}");
        assert!(
            r.requests.len()
                < aiplane_core::server::reasoning::Effort::Standard.max_rounds() as usize,
            "the guard, not the budget, ended the run"
        );
    }

    #[tokio::test]
    async fn a_final_round_that_writes_nothing_gets_a_gateway_account() {
        let r = run(vec![text("")], Some(contract()), "fast").await;
        let Some(RunOutcome::Incomplete { reason, summary }) = r.outcome else {
            panic!("expected an incomplete outcome, got {:?}", r.outcome);
        };
        assert!(matches!(
            reason,
            IncompleteReason::RoundBudgetExhausted { .. }
        ));
        assert!(summary.contains("without calling finish"), "{summary}");
    }

    #[tokio::test]
    async fn without_a_contract_a_run_behaves_as_before() {
        let r = run(vec![text("Done.")], None, "standard").await;
        assert_eq!(r.outcome, None);
        assert_eq!(r.requests.len(), 1);
        assert!(offered_tools(&r.requests[0]).is_empty());
        let system = r.requests[0]["messages"][0]["content"].as_str().unwrap();
        assert!(!system.contains("non-interactive run"), "{system}");
        assert_eq!(r.turn.turn.content.as_deref(), Some("Done."));
        assert_eq!(r.turn.turn.status, session_core::db::TurnStatus::Completed);
        assert_eq!(r.turn.turn.error_message, None);
    }

    #[tokio::test]
    async fn a_failed_run_is_incomplete_not_missing() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&upstream)
            .await;
        let state = state_for(&upstream.uri()).await;
        let (session_id, turn_id) = open(&state, "standard").await;
        let outcome = drive(&state, params(&session_id, &turn_id, Some(contract()))).await;
        let Some(RunOutcome::Incomplete {
            reason: IncompleteReason::Failed { message },
            ..
        }) = outcome
        else {
            panic!("expected a failed outcome, got {outcome:?}");
        };
        assert!(message.contains("500"), "{message}");
    }

    async fn run_budgeted(
        budget: Budget,
        tokens_per_round: Option<u64>,
        clock: Clock,
    ) -> (Option<RunOutcome>, Vec<Value>) {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(Scripted {
                deltas: vec![text("Still working.")],
                served: AtomicUsize::new(0),
                tokens_per_round,
            })
            .mount(&upstream)
            .await;
        let state = state_for(&upstream.uri()).await;
        let (session_id, turn_id) = open(&state, "max").await;
        let mut p = params(&session_id, &turn_id, Some(contract()));
        p.budget = Some(budget);
        let outcome = drive_with_clock(&state, p, clock).await;
        let requests = upstream
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|r| serde_json::from_slice(&r.body).unwrap())
            .collect();
        (outcome, requests)
    }

    /// Time is un-fakeable any other way (London-school seam): every read of
    /// this clock moves it forward by `step`, so "seconds elapsed" is a pure
    /// function of how many times the driver looked.
    fn stepping_clock(step: std::time::Duration) -> Clock {
        let base = std::time::Instant::now();
        let reads = Arc::new(AtomicUsize::new(0));
        Arc::new(move || base + step * reads.fetch_add(1, Ordering::SeqCst) as u32)
    }

    fn incomplete_reason(outcome: Option<RunOutcome>) -> IncompleteReason {
        match outcome {
            Some(RunOutcome::Incomplete { reason, .. }) => reason,
            other => panic!("expected an incomplete outcome, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_explicit_round_budget_overrides_the_effort_level() {
        let (outcome, requests) = run_budgeted(
            Budget::new(3, None, None),
            None,
            crate::budget::system_clock(),
        )
        .await;
        assert_eq!(
            incomplete_reason(outcome),
            IncompleteReason::RoundBudgetExhausted { rounds: 3 }
        );
        assert_eq!(requests.len(), 3);
    }

    #[tokio::test]
    async fn the_seconds_limit_ends_the_run_on_its_final_round() {
        let (outcome, requests) = run_budgeted(
            Budget::new(40, Some(30), None),
            None,
            stepping_clock(std::time::Duration::from_secs(10)),
        )
        .await;
        assert_eq!(
            incomplete_reason(outcome),
            IncompleteReason::SecondsExhausted { seconds: 30 }
        );
        assert_eq!(requests.len(), 3, "rounds were left, time was not");
        let last = requests.last().unwrap();
        assert_eq!(offered_tools(last), [FINISH_TOOL_NAME]);
    }

    #[tokio::test]
    async fn the_token_limit_ends_the_run_on_its_final_round() {
        let (outcome, requests) = run_budgeted(
            Budget::new(40, None, Some(250)),
            Some(100),
            crate::budget::system_clock(),
        )
        .await;
        assert_eq!(
            incomplete_reason(outcome),
            IncompleteReason::TokensExhausted { tokens: 250 }
        );
        assert_eq!(
            requests.len(),
            4,
            "the third round crosses 250; the fourth is final"
        );
        assert_eq!(offered_tools(requests.last().unwrap()), [FINISH_TOOL_NAME]);
    }

    #[tokio::test]
    async fn a_token_budget_asks_the_upstream_for_usage() {
        let (_, requests) = run_budgeted(
            Budget::new(1, None, Some(10)),
            Some(1),
            crate::budget::system_clock(),
        )
        .await;
        assert_eq!(requests[0]["stream_options"]["include_usage"], true);
    }
}
