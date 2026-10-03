// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! A `loop` route (#103), end to end: a main agent, a worker and a critic on
//! wiremock models, real SQLite and the real headless loop. It stops on the
//! critic's acceptance, on `max_iterations` and on the route's budget.

use super::*;
use crate::agents::run::draft::{collect_debug, run_draft_turn};

const VISITOR: &str = "Write me an offer. Private: my budget is 900 EUR.";

/// A streaming model that reports `tokens` spent with every answer.
async fn spending_llm(deltas: Vec<Value>, tokens: u64) -> MockServer {
    let served = AtomicUsize::new(0);
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(move |_: &wiremock::Request| {
            let i = served.fetch_add(1, Ordering::SeqCst).min(deltas.len() - 1);
            let sse = format!(
                "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                json!({"choices": [{"index": 0, "delta": deltas[i]}]}),
                json!({"choices": [], "usage": {
                    "prompt_tokens": tokens / 2, "completion_tokens": tokens / 2,
                    "total_tokens": tokens }}),
            );
            ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream")
        })
        .mount(&server)
        .await;
    server
}

fn worker_spec() -> Value {
    json!({
        "main": {
            "pool": "worker-pool",
            "instructions": { "orchestration": "Draft what the task asks for." },
            "budget": { "rounds": 4 }
        },
        "finish": { "schema": { "type": "object", "required": ["answer"],
                                "properties": { "answer": { "type": "string" } } } }
    })
}

fn critic_spec(schema: Value) -> Value {
    json!({
        "main": {
            "pool": "critic-pool",
            "instructions": { "orchestration": "Review the result against the task." },
            "budget": { "rounds": 4 }
        },
        "finish": { "schema": schema }
    })
}

fn verdict_schema() -> Value {
    json!({ "type": "object", "required": ["accepted"], "properties": {
        "accepted": { "type": "boolean" }, "feedback": { "type": "string" } } })
}

fn main_spec(worker: &str, critic: &str, extra: Value) -> Value {
    let mut target = json!({ "worker": worker, "critic": critic, "max_iterations": 3 });
    if let Value::Object(extra) = extra {
        target.as_object_mut().unwrap().extend(extra);
    }
    json!({
        "main": {
            "pool": "support-pool",
            "instructions": { "orchestration": "Collect the request, then call forward_request." },
            "budget": { "rounds": 6 }
        },
        "state": { "issue": { "type": "string", "max_length": 200, "set_by": ["llm"] } },
        "routes": {
            "offer": {
                "when": { "slot": "issue", "set": true },
                "task": "Write an offer for: {issue}",
                "loop": target
            }
        }
    })
}

struct Loop {
    world: World,
    main: MockServer,
    worker: MockServer,
    critic: MockServer,
    support: String,
    reply: AgentReply,
}

/// The main agent records the issue and forwards it once; the worker and
/// critic answer from `drafts` and `verdicts`.
async fn run_loop(
    drafts: Vec<Value>,
    verdicts: Vec<Value>,
    extra: Value,
    worker_tokens: u64,
) -> Loop {
    let main = llm(vec![
        calls(&[
            ("s1", "set_issue", json!({ "value": "a website redesign" })),
            ("fwd", "forward_request", json!({})),
        ]),
        text("Here is your offer."),
    ])
    .await;
    let worker = spending_llm(drafts, worker_tokens).await;
    let critic = llm(verdicts).await;
    let world = World::new(
        &[
            ("support-pool", "support-model", &main),
            ("worker-pool", "worker-model", &worker),
            ("critic-pool", "critic-model", &critic),
        ],
        None,
    )
    .await;
    let writer = world
        .agent("writer", &[(GrantKind::Pool, "worker-pool")])
        .await;
    world.publish(&writer, &worker_spec()).await;
    let reviewer = world
        .agent("reviewer", &[(GrantKind::Pool, "critic-pool")])
        .await;
    world
        .publish(&reviewer, &critic_spec(verdict_schema()))
        .await;
    let support = world
        .agent("support", &[(GrantKind::Pool, "support-pool")])
        .await;
    let spec = main_spec(&writer, &reviewer, extra);
    assert_eq!(world.issues(&support, &spec).await, []);
    world.publish(&support, &spec).await;
    let reply = run_turn(
        &world.state,
        AgentTurn {
            agent_id: &support,
            session_id: None,
            message: VISITOR,
            visitor_id: None,
            lang: None,
        },
    )
    .await
    .unwrap();
    Loop {
        world,
        main,
        worker,
        critic,
        support,
        reply,
    }
}

fn draft(id: &str, answer: &str) -> Value {
    finish(id, json!({ "answer": answer }))
}

fn verdict(id: &str, accepted: bool, feedback: &str) -> Value {
    finish(id, json!({ "accepted": accepted, "feedback": feedback }))
}

/// What `forward_request` answered the main model.
async fn forwarded(main: &MockServer) -> Value {
    serde_json::from_str(tool_answer(&requests(main).await, "fwd")).unwrap()
}

/// The task text each child run of `server` was given (its user message).
async fn tasks(server: &MockServer) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for request in requests(server).await {
        let task = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "user")
            .and_then(|m| m["content"].as_str())
            .unwrap_or_default()
            .to_string();
        if !seen.contains(&task) {
            seen.push(task);
        }
    }
    seen
}

#[tokio::test]
async fn the_loop_revises_until_the_critic_accepts_and_returns_the_last_draft() {
    let run = run_loop(
        vec![
            draft("w1", "Offer v1"),
            draft("w2", "Offer v2: 3 pages, 2 weeks"),
        ],
        vec![
            verdict("c1", false, "Say how long it takes."),
            verdict("c2", true, ""),
        ],
        json!({}),
        0,
    )
    .await;
    assert_eq!(run.reply.status, chat::TurnStatus::Completed);
    assert_eq!(run.reply.answer.as_deref(), Some("Here is your offer."));

    let seen = forwarded(&run.main).await;
    assert_eq!(seen["forwarded"], true);
    assert_eq!(seen["loop"]["iterations"], 2);
    assert_eq!(seen["loop"]["accepted"], true);
    assert_eq!(seen["loop"]["stopped"], "accepted");
    assert_eq!(
        seen["outcome"],
        json!({ "status": "finished", "result": { "answer": "Offer v2: 3 pages, 2 weeks" } })
    );

    let worker_tasks = tasks(&run.worker).await;
    assert_eq!(worker_tasks.len(), 2);
    assert_eq!(worker_tasks[0], "Write an offer for: a website redesign");
    assert!(
        worker_tasks[1].contains("Offer v1") && worker_tasks[1].contains("Say how long it takes.")
    );
    let critic_tasks = tasks(&run.critic).await;
    assert_eq!(critic_tasks.len(), 2);
    assert!(critic_tasks[0].contains("Write an offer for: a website redesign"));
    assert!(critic_tasks[0].contains("Offer v1"));
    for request in requests(&run.worker)
        .await
        .iter()
        .chain(&requests(&run.critic).await)
    {
        assert!(
            !request.to_string().contains("900 EUR"),
            "the children never see the transcript"
        );
    }

    let audit = run.world.audit(&run.support).await;
    let kinds: Vec<&str> = audit.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == "sub_agent_dispatched")
            .count(),
        4
    );
    assert_eq!(kinds.iter().filter(|k| **k == "loop_iteration").count(), 2);
    let finished = audit.iter().find(|e| e.kind == "loop_finished").unwrap();
    assert_eq!(finished.detail["stopped"], "accepted");
    assert_eq!(finished.detail["iterations"], 2);
    assert!(finished.chain.is_some());
    let first = audit.iter().find(|e| e.kind == "loop_iteration").unwrap();
    assert_eq!(first.detail["accepted"], false);
    assert_eq!(first.detail["feedback"], "Say how long it takes.");
}

#[tokio::test]
async fn the_loop_stops_after_max_iterations_with_the_last_draft_unaccepted() {
    let run = run_loop(
        vec![draft("w1", "Offer v1"), draft("w2", "Offer v2")],
        vec![verdict("c1", false, "Still too vague.")],
        json!({ "max_iterations": 2 }),
        0,
    )
    .await;
    let seen = forwarded(&run.main).await;
    assert_eq!(seen["loop"]["iterations"], 2);
    assert_eq!(seen["loop"]["accepted"], false);
    assert_eq!(seen["loop"]["stopped"], "max_iterations");
    assert_eq!(seen["outcome"]["result"]["answer"], "Offer v2");
    assert_eq!(tasks(&run.worker).await.len(), 2);
}

#[tokio::test]
async fn the_loop_stops_once_its_budget_is_spent_across_child_runs() {
    let run = run_loop(
        vec![draft("w1", "Offer v1")],
        vec![verdict("c1", false, "More detail.")],
        json!({ "budget": { "tokens": 500 } }),
        600,
    )
    .await;
    let seen = forwarded(&run.main).await;
    assert_eq!(seen["loop"]["stopped"], "budget");
    assert_eq!(seen["loop"]["iterations"], 1);
    assert_eq!(seen["outcome"]["result"]["answer"], "Offer v1");
    assert!(
        requests(&run.critic).await.is_empty(),
        "the critic never ran once the worker spent the route's tokens"
    );
    let finished = run
        .world
        .audit(&run.support)
        .await
        .into_iter()
        .find(|e| e.kind == "loop_finished")
        .unwrap();
    assert_eq!(finished.detail["tokens"], 600);
}

#[tokio::test]
async fn a_worker_that_does_not_finish_ends_the_loop_incomplete() {
    let run = run_loop(
        vec![text("I would rather chat than finish.")],
        vec![verdict("c1", true, "")],
        json!({}),
        0,
    )
    .await;
    let seen = forwarded(&run.main).await;
    assert_eq!(seen["loop"]["stopped"], "worker_incomplete");
    assert_eq!(seen["outcome"]["status"], "incomplete");
    assert!(requests(&run.critic).await.is_empty());
}

#[tokio::test]
async fn the_test_chat_debug_view_shows_every_iteration() {
    let main = llm(vec![
        calls(&[
            ("s1", "set_issue", json!({ "value": "a website redesign" })),
            ("fwd", "forward_request", json!({})),
        ]),
        text("Here is your offer."),
    ])
    .await;
    let worker = llm(vec![draft("w1", "Offer v1"), draft("w2", "Offer v2")]).await;
    let critic = llm(vec![
        verdict("c1", false, "Shorter."),
        verdict("c2", true, ""),
    ])
    .await;
    let world = World::new(
        &[
            ("support-pool", "support-model", &main),
            ("worker-pool", "worker-model", &worker),
            ("critic-pool", "critic-model", &critic),
        ],
        None,
    )
    .await;
    let writer = world
        .agent("writer", &[(GrantKind::Pool, "worker-pool")])
        .await;
    world.publish(&writer, &worker_spec()).await;
    let reviewer = world
        .agent("reviewer", &[(GrantKind::Pool, "critic-pool")])
        .await;
    world
        .publish(&reviewer, &critic_spec(verdict_schema()))
        .await;
    let support = world
        .agent("support", &[(GrantKind::Pool, "support-pool")])
        .await;
    let draft_spec = main_spec(&writer, &reviewer, json!({}));
    let since = jiff::Timestamp::now();
    let reply = run_draft_turn(
        &world.state,
        AgentTurn {
            agent_id: &support,
            session_id: None,
            message: VISITOR,
            visitor_id: None,
            lang: None,
        },
        &draft_spec,
        RunOptions::default(),
    )
    .await
    .unwrap();
    let debug = collect_debug(
        &world.state,
        &support,
        &draft_spec,
        &reply.session_id,
        since,
        &RunOptions::default(),
    )
    .await
    .unwrap();
    let steps: Vec<(String, u64)> = debug
        .sub_agents
        .iter()
        .map(|s| {
            (
                s["loop"]["role"].as_str().unwrap().to_string(),
                s["loop"]["iteration"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        steps,
        [
            ("worker".to_string(), 1),
            ("critic".to_string(), 1),
            ("worker".to_string(), 2),
            ("critic".to_string(), 2)
        ]
    );
    assert!(
        debug
            .sub_agents
            .iter()
            .all(|s| s["outcome"]["status"] == "finished")
    );
    let events: Vec<&str> = debug
        .loops
        .iter()
        .map(|l| l["event"].as_str().unwrap())
        .collect();
    assert_eq!(
        events,
        ["loop_iteration", "loop_iteration", "loop_finished"]
    );
    assert_eq!(debug.loops[2]["stopped"], "accepted");
}

#[tokio::test]
async fn the_validator_refuses_a_critic_without_an_acceptance_field_and_loops_back() {
    let main = llm(vec![text("unused")]).await;
    let world = World::new(&[("support-pool", "support-model", &main)], None).await;
    let writer = world
        .agent("writer", &[(GrantKind::Pool, "support-pool")])
        .await;
    world.publish(&writer, &worker_spec()).await;
    let lax = world
        .agent("lax", &[(GrantKind::Pool, "support-pool")])
        .await;
    world
        .publish(
            &lax,
            &critic_spec(json!({ "type": "object", "properties": {
                "accepted": { "type": "string" } } })),
        )
        .await;
    let support = world
        .agent("support", &[(GrantKind::Pool, "support-pool")])
        .await;

    let issues = world
        .issues(&support, &main_spec(&writer, &lax, json!({})))
        .await;
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(issues[0].path, "routes.offer.loop.critic");
    assert!(
        issues[0].message.contains("required boolean `accepted`"),
        "{}",
        issues[0].message
    );

    let same = world
        .issues(&support, &main_spec(&writer, &writer, json!({})))
        .await;
    assert!(
        same.iter()
            .any(|i| i.path == "routes.offer.loop.critic" && i.message.contains("another agent")),
        "{same:?}"
    );
    let itself = world
        .issues(&support, &main_spec(&support, &writer, json!({})))
        .await;
    assert!(
        itself
            .iter()
            .any(|i| i.path == "routes.offer.loop.worker" && i.message.contains("itself")),
        "{itself:?}"
    );

    // A critic that routes back to the main agent closes a cycle.
    let routing_back = world
        .agent("routing-back", &[(GrantKind::Pool, "support-pool")])
        .await;
    let mut back = critic_spec(verdict_schema());
    back["state"] = json!({ "issue": { "type": "string", "set_by": ["llm"] } });
    back["routes"] = json!({ "back": {
        "when": { "slot": "issue", "set": true }, "agent": support, "task": "{issue}" } });
    world.publish(&routing_back, &back).await;
    let looped = world
        .issues(&support, &main_spec(&writer, &routing_back, json!({})))
        .await;
    assert!(
        looped
            .iter()
            .any(|i| i.path == "routes.offer.loop.critic" && i.message.contains("loops back")),
        "{looped:?}"
    );

    let shapes = world
        .issues(
            &support,
            &main_spec(
                &writer,
                &lax,
                json!({ "max_iterations": 0, "budget": { "rounds": 3 } }),
            ),
        )
        .await;
    let paths: Vec<&str> = shapes.iter().map(|i| i.path.as_str()).collect();
    assert!(
        paths.contains(&"routes.offer.loop.max_iterations"),
        "{paths:?}"
    );
    assert!(
        paths.contains(&"routes.offer.loop.budget.rounds"),
        "{paths:?}"
    );
}
