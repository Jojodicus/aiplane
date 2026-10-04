// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The topic guard of a strict scope (`docs/agents.md` "What #115 built"),
//! and the structured system prompt every agent run gets. The real case: an
//! agent told to "deny questions outside your scope" with no scope defined
//! answered a question about diesel engines in detail.

use aiplane_agents::db::agent_audit::{ActivityQuery, AuditKind, Order};

use super::*;

const REFUSAL: &str = "I can only help with croit products and Ceph storage.";
const DIESEL: &str = "How does the injection pump of a diesel engine work?";

fn website_spec(strict: bool) -> Value {
    json!({
        "profile": { "display": "croit Website Assistant" },
        "scope": {
            "topics": ["croit products", "Ceph storage"],
            "refusal": REFUSAL,
            "strict": strict,
            "classifier_model": "guard-model"
        },
        "main": {
            "model": "website-model",
            "instructions": {
                "orchestration": "Answer questions about croit and Ceph.",
                "response": "Friendly and short."
            },
            "budget": { "rounds": 3 }
        }
    })
}

/// A non-streaming guard upstream answering `{"verdict": …}` in turn.
async fn guard(verdicts: &[&str]) -> MockServer {
    let answers: Vec<String> = verdicts
        .iter()
        .map(|v| json!({ "verdict": v }).to_string())
        .collect();
    let served = AtomicUsize::new(0);
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(move |_: &wiremock::Request| {
            let i = served.fetch_add(1, Ordering::SeqCst).min(answers.len() - 1);
            ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{ "message": { "role": "assistant", "content": answers[i] } }],
                "usage": { "prompt_tokens": 90, "completion_tokens": 6, "total_tokens": 96 }
            }))
        })
        .mount(&server)
        .await;
    server
}

struct Website {
    world: World,
    agent: String,
}

async fn website(spec: Value, main: &MockServer, guard: &MockServer, metered: bool) -> Website {
    let pools = [
        ("website-pool", "website-model", main),
        ("guard-pool", "guard-model", guard),
    ];
    let world = if metered {
        World::metered(&pools).await
    } else {
        World::new(&pools, None).await
    };
    let agent = world
        .agent(
            "website",
            &[
                (GrantKind::Model, "website-model"),
                (GrantKind::Model, "guard-model"),
            ],
        )
        .await;
    assert_eq!(world.issues(&agent, &spec).await, []);
    world.publish(&agent, &spec).await;
    Website { world, agent }
}

impl Website {
    async fn says(&self, session: Option<&str>, message: &str) -> AgentReply {
        run_turn(
            &self.world.state,
            AgentTurn {
                agent_id: &self.agent,
                session_id: session,
                message,
                visitor_id: None,
                lang: None,
            },
        )
        .await
        .unwrap()
    }

    /// The run's activity of `kind`, oldest first, content kinds included.
    async fn events(&self, kind: AuditKind) -> Vec<Value> {
        agent_audit::page(
            self.world.db(),
            &ActivityQuery {
                agent_id: self.agent.clone(),
                kinds: vec![kind],
                order: Order::Asc,
                limit: 1_000,
                max_bytes: usize::MAX,
                ..ActivityQuery::default()
            },
        )
        .await
        .unwrap()
        .events
        .into_iter()
        .map(|e| serde_json::from_str(&e.detail).unwrap())
        .collect()
    }
}

#[tokio::test]
async fn an_off_topic_question_gets_the_refusal_and_never_reaches_the_main_model() {
    let main = llm(vec![text("The injection pump pressurises the fuel …")]).await;
    let guard = guard(&["out_of_scope"]).await;
    let site = website(website_spec(true), &main, &guard, true).await;

    let reply = site.says(None, DIESEL).await;

    assert_eq!(reply.answer.as_deref(), Some(REFUSAL));
    assert_eq!(reply.status, chat::TurnStatus::Completed);
    assert!(
        requests(&main).await.is_empty(),
        "the main model is never called"
    );
    let asked = requests(&guard).await;
    assert_eq!(asked.len(), 1);
    assert_eq!(
        asked[0]["response_format"]["json_schema"]["schema"]["properties"]["verdict"]["enum"],
        json!(["in_scope", "out_of_scope"])
    );
    let input: Value =
        serde_json::from_str(asked[0]["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(input["latest_message"], DIESEL);
    assert_eq!(input["topics"], json!(["croit products", "Ceph storage"]));

    let decisions = site.events(AuditKind::ScopeDecision).await;
    assert_eq!(decisions.len(), 1, "{decisions:?}");
    assert_eq!(decisions[0]["verdict"], "out_of_scope");
    assert_eq!(decisions[0]["model"], "guard-model");
    let exchanges = site.events(AuditKind::LlmExchange).await;
    assert_eq!(exchanges.len(), 1, "only the guard's call: {exchanges:?}");
    assert_eq!(exchanges[0]["purpose"], "scope_guard");
    assert_eq!(exchanges[0]["answer"], "out_of_scope");

    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    let metered: Vec<(String, Option<i64>)> =
        sqlx::query_as("SELECT model, total_tokens FROM usage_events")
            .fetch_all(site.world.db())
            .await
            .unwrap();
    assert_eq!(
        metered,
        [("guard-model".to_string(), Some(96))],
        "the guard's call counts against the agent's budget and limits"
    );
}

#[tokio::test]
async fn an_in_scope_question_reaches_the_main_model_under_the_structured_prompt() {
    let main = llm(vec![text("A 3-node croit cluster starts at …")]).await;
    let guard = guard(&["in_scope"]).await;
    let site = website(website_spec(true), &main, &guard, false).await;

    let reply = site
        .says(None, "What does a 3-node Ceph cluster with croit cost?")
        .await;

    assert_eq!(
        reply.answer.as_deref(),
        Some("A 3-node croit cluster starts at …")
    );
    let sent = requests(&main).await;
    assert_eq!(sent.len(), 1);
    assert_eq!(
        system(&sent[0]),
        "## Role\n\nYou are croit Website Assistant. The sections below are your owner's \
         instructions.\n\n## Task\n\nAnswer questions about croit and Ceph.\n\n## Scope\n\nYou \
         cover only these topics:\n- croit products\n- Ceph storage\nIf asked about anything \
         else, reply exactly: I can only help with croit products and Ceph storage.\n\n## \
         Tone\n\nFriendly and short."
    );
    assert_eq!(
        site.events(AuditKind::ScopeDecision).await[0]["verdict"],
        "in_scope"
    );
}

#[tokio::test]
async fn a_follow_up_is_judged_with_the_exchange_before_it() {
    let main = llm(vec![
        text("About 20k per year."),
        text("Support is included."),
    ])
    .await;
    let guard = guard(&["in_scope"]).await;
    let site = website(website_spec(true), &main, &guard, false).await;

    let first = site
        .says(None, "What does croit cost for three nodes?")
        .await;
    let second = site
        .says(Some(&first.session_id), "and what about support?")
        .await;

    assert_eq!(second.answer.as_deref(), Some("Support is included."));
    let asked = requests(&guard).await;
    assert_eq!(asked.len(), 2, "every visitor message is judged");
    let input: Value =
        serde_json::from_str(asked[1]["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(input["latest_message"], "and what about support?");
    assert_eq!(
        input["previous_exchange"],
        json!([
            { "visitor": "What does croit cost for three nodes?" },
            { "assistant": "About 20k per year." }
        ])
    );
}

#[tokio::test]
async fn a_scope_that_is_not_strict_is_guidance_in_the_prompt_and_nothing_more() {
    let main = llm(vec![text("Diesel engines compress air …")]).await;
    let guard = guard(&["out_of_scope"]).await;
    let site = website(website_spec(false), &main, &guard, false).await;

    let reply = site.says(None, DIESEL).await;

    assert_eq!(
        reply.answer.as_deref(),
        Some("Diesel engines compress air …")
    );
    assert!(
        requests(&guard).await.is_empty(),
        "no guard without `strict`"
    );
    let prompt = system(&requests(&main).await[0]).to_string();
    assert!(
        prompt.contains(&format!(
            "If asked about anything else, reply exactly: {REFUSAL}"
        )),
        "{prompt}"
    );
    assert!(
        prompt.starts_with("## Role\n\nYou are croit Website Assistant."),
        "{prompt}"
    );
    assert!(
        !prompt.contains("`website`"),
        "the agent is named, not its slug: {prompt}"
    );
    assert!(site.events(AuditKind::ScopeDecision).await.is_empty());
}

#[tokio::test]
async fn a_guard_that_cannot_decide_refuses_and_records_why() {
    let main = llm(vec![text("Diesel engines compress air …")]).await;
    let broken = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&broken)
        .await;
    let site = website(website_spec(true), &main, &broken, false).await;

    let reply = site.says(None, DIESEL).await;

    assert_eq!(
        reply.answer.as_deref(),
        Some(REFUSAL),
        "the guard fails closed"
    );
    assert!(requests(&main).await.is_empty());
    let decisions = site.events(AuditKind::ScopeDecision).await;
    assert_eq!(decisions[0]["verdict"], "failed");
    assert!(
        decisions[0]["error"].as_str().unwrap().contains("503"),
        "{decisions:?}"
    );
}

#[tokio::test]
async fn a_routed_sub_agent_is_never_guarded() {
    let guard = guard(&["out_of_scope"]).await;
    let sub = llm(vec![finish("f1", json!({ "answer": "done" }))]).await;
    let world = World::new(
        &[
            ("sub-pool", "sub-model", &sub),
            ("guard-pool", "guard-model", &guard),
        ],
        None,
    )
    .await;
    let mut spec = website_spec(true);
    spec["main"]["model"] = json!("sub-model");
    spec["finish"] = json!({ "schema": { "type": "object", "required": ["answer"],
                                         "properties": { "answer": { "type": "string" } } } });
    let id = world
        .agent(
            "helper",
            &[
                (GrantKind::Model, "sub-model"),
                (GrantKind::Model, "guard-model"),
            ],
        )
        .await;
    world.publish(&id, &spec).await;

    let profile = RunProfile::load(
        &world.state,
        &id,
        Role::SubAgent {
            route_binds: Default::default(),
        },
        &RunOptions::default(),
    )
    .await
    .unwrap();

    assert!(profile.surface.topic_guard().is_none());
}
