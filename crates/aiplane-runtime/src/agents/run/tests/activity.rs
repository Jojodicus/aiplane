// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The activity log (#111) over whole runs: a main agent whose gate is
//! closed until an OTP verifier opens it, a sub-agent, an external A2A
//! agent with a sealed credential, a handoff to a person and the answer —
//! every model exchange, tool call, state write and decision in one hash
//! chain per conversation, enough to reconstruct the run, and no secret in
//! the database. Then the log under parallel turns, and a run whose log
//! cannot be written.

use aiplane_agents::db::agent_audit::{ActivityQuery, Order, StoredEvent};
use aiplane_core::server::config::NetworkConfig;
use session_core::db::{Decision, SuspensionKind};

use super::a2a::{Peer, TOKEN, completed, finish_schema};
use super::suspend::{answer, every_stored_text, staff, visitor_says};
use super::verifiers::{ALICE, CODE, erp};
use super::*;
use crate::agents::a2a_client as a2a;
use crate::agents::resume::{AgentResume, ResumedBy, claim, run_claimed};

const VISITOR: &str = "My March invoice is wrong and I want to know about my warranty.";
const QUESTION: &str = "May we refund the duplicate charge on RE-1?";
const STAFF_ANSWER: &str = "Yes, refund it today.";
const FINAL: &str = "RE-1 was billed twice and is refunded today; you are covered until 2027.";

fn billing_spec() -> Value {
    json!({
        "main": {
            "pool": "billing-pool",
            "instructions": { "orchestration": "Explain the customer's invoice." },
            "budget": { "rounds": 3 }
        },
        "finish": { "schema": finish_schema() }
    })
}

fn support_spec(billing: &str, card_url: &str) -> Value {
    let verified = json!({ "slot": "verified", "provenance": "verifier:otp" });
    json!({
        "main": {
            "pool": "support-pool",
            "instructions": { "orchestration": "Verify the visitor, then route the request." },
            "budget": { "rounds": 12 }
        },
        "state": {
            "email": { "type": "email", "set_by": ["llm"] },
            "issue": { "type": "enum", "values": ["billing", "warranty"], "set_by": ["llm"] },
            "verified": { "type": "subject", "set_by": ["verifier:otp"] }
        },
        "verifiers": { "otp": { "kind": "mcp_code", "connector": "erp", "email_slot": "email",
                                "writes": { "verified": "result" } } },
        "router": { "kind": "rules", "order": ["billing", "partner"] },
        "routes": {
            "billing": {
                "when": { "all": [ { "slot": "issue", "eq": "billing" }, verified ] },
                "agent": billing,
                "task": "Invoice question of customer {verified.customer_id}"
            },
            "partner": {
                "when": { "all": [ { "slot": "issue", "eq": "warranty" }, verified ] },
                "task": "Warranty of customer {verified.customer_id}",
                "bind": { "customer": "state.verified.customer_id" },
                "a2a": {
                    "card_url": card_url,
                    "auth": { "kind": "bearer", "token": TOKEN },
                    "finish": { "schema": finish_schema() },
                    "budget": { "seconds": 20 }
                }
            },
            "desk": { "when": verified, "human": { "inbox": "billing desk" } }
        }
    })
}

struct Run {
    world: World,
    support: String,
    billing: String,
    main: Vec<Value>,
    sub: Vec<Value>,
    session: String,
    done: AgentReply,
}

/// The whole support story, from the visitor's first message to the answer
/// after a person decided.
async fn the_story() -> Run {
    let main = llm(vec![
        calls(&[
            ("k1", "set_email", json!({ "value": ALICE })),
            ("k2", "set_issue", json!({ "value": "billing" })),
        ]),
        call("fwd1", "forward_request", json!({})),
        call("v1", "verify_otp_request_code", json!({})),
        call("fwd2", "forward_request", json!({})),
        call("k3", "set_issue", json!({ "value": "warranty" })),
        call("fwd3", "forward_request", json!({})),
        call("h1", "request_human", json!({ "question": QUESTION })),
        text(FINAL),
    ])
    .await;
    let sub = llm(vec![finish(
        "b1",
        json!({ "answer": "RE-1 was billed twice." }),
    )])
    .await;
    let peer = Peer::bearer(vec![completed(json!({ "answer": "Covered until 2027." }))]).await;
    let (erp, _) = erp().await;
    let world = World::build_with(
        &[
            ("support-pool", "support-model", &main),
            ("billing-pool", "billing-model", &sub),
        ],
        None,
        false,
        base_tools(),
        None,
        NetworkConfig {
            allow_private_networks: true,
        },
    )
    .await;
    world.connect_erp(&erp).await;
    let billing = world
        .agent("billing", &[(GrantKind::Pool, "billing-pool")])
        .await;
    world.publish(&billing, &billing_spec()).await;
    let support = world
        .agent(
            "support",
            &[
                (GrantKind::Pool, "support-pool"),
                (GrantKind::Connector, "erp"),
                (GrantKind::A2aAgent, &peer.card_url()),
            ],
        )
        .await;
    let mut spec = support_spec(&billing, &peer.card_url());
    a2a::seal_secrets(&mut spec, &world.state.crypto).unwrap();
    assert_eq!(world.issues(&support, &spec).await, []);
    world.publish(&support, &spec).await;

    let paused = visitor_says(&world, &support, VISITOR).await;
    let waiting = paused.suspension.clone().expect("the code is asked for");
    assert_eq!(waiting.kind, SuspensionKind::SecureInput);
    let claimed = claim(
        &world.state,
        AgentResume {
            agent_id: &support,
            ..answer(
                &paused,
                Decision::Value { value: json!(CODE) },
                ResumedBy::Participant,
            )
        },
    )
    .await
    .unwrap();
    let handed_off = run_claimed(&world.state, claimed, RunOptions::default())
        .await
        .unwrap();
    let waiting = handed_off
        .suspension
        .clone()
        .expect("a person is asked next");
    assert_eq!(waiting.kind, SuspensionKind::HumanAnswer);
    let claimed = claim(
        &world.state,
        AgentResume {
            agent_id: &support,
            ..answer(
                &handed_off,
                Decision::Value {
                    value: json!(STAFF_ANSWER),
                },
                staff(),
            )
        },
    )
    .await
    .unwrap();
    let done = run_claimed(&world.state, claimed, RunOptions::default())
        .await
        .unwrap();
    assert_eq!(done.status, chat::TurnStatus::Completed);
    assert_eq!(done.answer.as_deref(), Some(FINAL));
    Run {
        main: requests(&main).await,
        sub: requests(&sub).await,
        session: paused.session_id.clone(),
        world,
        support,
        billing,
        done,
    }
}

/// Every event of the conversation, oldest first.
async fn conversation(run: &Run) -> Vec<StoredEvent> {
    agent_audit::page(
        run.world.db(),
        &ActivityQuery {
            agent_id: run.support.clone(),
            conversation_id: Some(run.session.clone()),
            order: Order::Asc,
            limit: 10_000,
            max_bytes: usize::MAX,
            ..ActivityQuery::default()
        },
    )
    .await
    .unwrap()
    .events
}

fn detail(e: &StoredEvent) -> Value {
    serde_json::from_str(&e.detail).unwrap()
}

fn position(events: &[StoredEvent], what: impl Fn(&StoredEvent, &Value) -> bool) -> usize {
    events
        .iter()
        .position(|e| what(e, &detail(e)))
        .expect("the event is in the log")
}

#[tokio::test]
async fn a_whole_run_is_one_hash_chain_that_reconstructs_it_and_holds_no_secret() {
    let run = the_story().await;
    let events = conversation(&run).await;

    let verified = agent_audit::verify(run.world.db(), &run.support)
        .await
        .unwrap();
    assert!(verified.ok(), "{verified:?}");
    let seqs: Vec<i64> = events.iter().map(|e| e.seq.unwrap()).collect();
    assert_eq!(
        seqs,
        (1..=events.len() as i64).collect::<Vec<_>>(),
        "one chain, in the order things happened"
    );
    assert!(
        events
            .iter()
            .all(|e| e.chain_key.as_deref() == Some(&format!("conversation:{}", run.session)))
    );

    // Every model request, exactly as sent, and what came back.
    let exchanges = |principal: &str| -> Vec<Value> {
        events
            .iter()
            .filter(|e| e.kind == "llm_exchange" && e.principal_id == principal)
            .map(detail)
            .collect()
    };
    let main_exchanges = exchanges(&run.support);
    assert_eq!(
        main_exchanges
            .iter()
            .map(|e| e["request"].clone())
            .collect::<Vec<_>>(),
        run.main,
        "the main agent's requests, byte for byte"
    );
    assert_eq!(
        exchanges(&run.billing)
            .iter()
            .map(|e| e["request"].clone())
            .collect::<Vec<_>>(),
        run.sub,
        "the sub-agent's, in the same chain"
    );
    assert_eq!(main_exchanges.last().unwrap()["response"]["content"], FINAL);
    assert_eq!(
        main_exchanges[0]["response"]["tool_calls"][0]["function"]["name"],
        "set_email"
    );

    // Every tool call the main model made has its full result.
    let results: HashMap<String, Value> = events
        .iter()
        .filter(|e| e.kind == "tool_result")
        .map(|e| (e.call_id.clone().unwrap(), detail(e)))
        .collect();
    for id in ["k1", "k2", "fwd1", "v1", "fwd2", "k3", "fwd3", "h1", "b1"] {
        assert!(results.contains_key(id), "no tool_result for {id}");
    }
    assert_eq!(results["k1"]["arguments"], json!({ "value": ALICE }));
    assert_eq!(results["fwd1"]["result"]["reason"], "no_open_route");
    assert_eq!(results["fwd2"]["result"]["outcome"]["status"], "finished");
    assert_eq!(
        results["fwd3"]["result"]["outcome"]["result"]["answer"],
        "Covered until 2027."
    );

    // The turns: what the visitor said, and the answer.
    let started = position(&events, |e, d| {
        e.kind == "turn_started" && d["message"] == VISITOR
    });
    assert_eq!(started, 0);
    let last = events.last().unwrap();
    assert_eq!(last.kind, "turn_finished");
    assert_eq!(detail(last)["answer"], FINAL);
    assert_eq!(last.turn_id.as_deref(), Some(run.done.turn_id.as_str()));

    // The story in order: closed gate, code asked, code received, the
    // verifier's slot written and its outcome, sub-agent, external agent, a
    // person, the answer.
    let order = [
        position(&events, |e, d| {
            e.kind == "route_decision" && d["reason"] == "no_open_route"
        }),
        position(&events, |e, d| {
            e.kind == "run_suspended" && d["kind"] == "secure_input"
        }),
        position(&events, |e, d| {
            e.kind == "run_resumed" && d["secure_input_received"] == true
        }),
        position(&events, |e, d| {
            e.kind == "state_written" && d["slot"] == "verified"
        }),
        position(&events, |e, d| {
            e.kind == "verifier_outcome" && d["outcome"] == "verified"
        }),
        position(&events, |e, d| {
            e.kind == "sub_agent_dispatched" && d["route"] == "billing"
        }),
        position(&events, |e, _| {
            e.kind == "turn_started" && e.principal_id == run.billing
        }),
        position(&events, |e, d| {
            e.kind == "sub_agent_finished" && d["route"] == "billing"
        }),
        position(&events, |e, d| {
            e.kind == "sub_agent_dispatched" && d["target"] == "a2a"
        }),
        position(&events, |e, d| {
            e.kind == "human_handoff" && d["route"] == "desk"
        }),
        position(&events, |e, d| {
            e.kind == "run_resumed" && d["answer"] == STAFF_ANSWER
        }),
        events.len() - 1,
    ];
    assert!(
        order.windows(2).all(|w| w[0] < w[1]),
        "the events follow the run: {order:?}"
    );

    let issue_writes: Vec<Value> = events
        .iter()
        .filter(|e| e.kind == "state_written" && detail(e)["slot"] == "issue")
        .map(detail)
        .collect();
    assert_eq!(issue_writes.len(), 2);
    assert_eq!(issue_writes[1]["old"]["value"], "billing");
    assert_eq!(issue_writes[1]["new"], "warranty");
    assert_eq!(issue_writes[1]["provenance"], "llm");

    let sub_event = events
        .iter()
        .find(|e| e.principal_id == run.billing)
        .unwrap();
    assert_eq!(sub_event.agent_id.as_deref(), Some(run.support.as_str()));
    let chain: Value = serde_json::from_str(sub_event.chain.as_deref().unwrap()).unwrap();
    assert_eq!(chain["frames"].as_array().unwrap().len(), 2);

    let sent = detail(
        &events[position(&events, |e, d| {
            e.kind == "sub_agent_dispatched" && d["target"] == "a2a"
        })],
    );
    assert_eq!(
        sent["message"]["parts"][1]["data"],
        json!({ "customer": "K-1" }),
        "what left the gateway for the external agent"
    );

    let everything = every_stored_text(run.world.db()).await;
    assert!(!everything.contains(CODE), "the one-time code is nowhere");
    assert!(
        !everything.contains(TOKEN),
        "the external agent's credential is stored sealed only"
    );
    let resumed_secure = detail(&events[order[2]]);
    assert!(resumed_secure.get("answer").is_none());
}

#[tokio::test]
async fn a_changed_event_breaks_the_chain_at_that_event() {
    let run = the_story().await;
    let events = conversation(&run).await;
    let tampered = &events[events.len() / 2];
    sqlx::query("UPDATE agent_audit SET detail = '{}' WHERE id = ?")
        .bind(&tampered.id)
        .execute(run.world.db())
        .await
        .unwrap();
    let broken = agent_audit::verify(run.world.db(), &run.support)
        .await
        .unwrap()
        .broken
        .expect("the change is found");
    assert_eq!(broken.event_id.as_deref(), Some(tampered.id.as_str()));
    assert_eq!(Some(broken.seq), tampered.seq);
}

/// Answers a round that has a tool result with text, any other with a call
/// to the echo tool — the same for every conversation, whatever order their
/// rounds arrive in.
struct EchoThenAnswer;

impl wiremock::Respond for EchoThenAnswer {
    fn respond(&self, req: &wiremock::Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let answered = body["messages"]
            .as_array()
            .unwrap()
            .last()
            .is_some_and(|m| m["role"] == "tool");
        let delta = if answered {
            text("done")
        } else {
            call("e1", "company_echo", json!({ "message": "ping" }))
        };
        let sse = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"choices": [{"index": 0, "delta": delta}]})
        );
        ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream")
    }
}

/// An agent that echoes once and answers, on a database file (parallel
/// writers need real locking), which lives as long as the returned dir.
async fn echoing_agent() -> (World, String, MockServer, tempfile::TempDir) {
    let main = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(EchoThenAnswer)
        .mount(&main)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let world = World::build(
        &[("support-pool", "support-model", &main)],
        None,
        false,
        base_tools(),
        Some(&dir.path().join("db.sqlite")),
    )
    .await;
    let agent = world
        .agent(
            "support",
            &[
                (GrantKind::Pool, "support-pool"),
                (GrantKind::Tool, "company_echo"),
            ],
        )
        .await;
    world
        .publish(
            &agent,
            &json!({ "main": {
                "pool": "support-pool",
                "instructions": { "orchestration": "Echo, then answer." },
                "tools": ["company_echo"],
                "budget": { "rounds": 4 }
            } }),
        )
        .await;
    (world, agent, main, dir)
}

#[tokio::test]
async fn parallel_turns_lose_no_event_and_keep_every_chain_whole() {
    let (world, agent, main, _dir) = echoing_agent().await;
    let world = Arc::new(world);
    let turns: Vec<_> = (0..6)
        .map(|n| {
            let (world, agent) = (world.clone(), agent.clone());
            tokio::spawn(async move {
                let message = format!("visitor {n}");
                visitor_says(&world, &agent, &message).await
            })
        })
        .collect();
    let mut sessions = Vec::new();
    for turn in turns {
        let reply = turn.await.unwrap();
        assert_eq!(reply.status, chat::TurnStatus::Completed);
        sessions.push(reply.session_id);
    }

    let verified = agent_audit::verify(world.db(), &agent).await.unwrap();
    assert!(verified.ok(), "{verified:?}");
    for session in &sessions {
        let kinds: Vec<String> = agent_audit::page(
            world.db(),
            &ActivityQuery {
                agent_id: agent.clone(),
                conversation_id: Some(session.clone()),
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
        .map(|e| e.kind)
        .collect();
        assert_eq!(
            kinds,
            [
                "turn_started",
                "llm_exchange",
                "tool_call",
                "tool_result",
                "llm_exchange",
                "turn_finished"
            ],
            "conversation {session}"
        );
    }
    assert_eq!(
        requests(&main).await.len(),
        12,
        "every exchange the upstream saw is in the log"
    );
}

/// The log refuses every write of `kind` (every kind for `None`), as a full
/// disk or a broken database would.
async fn refuse_writes(world: &World, kind: Option<&str>) {
    let when = kind.map_or(String::new(), |k| format!("WHEN NEW.kind = '{k}'"));
    sqlx::query(&format!(
        "CREATE TRIGGER refuse_activity BEFORE INSERT ON agent_audit {when}
         BEGIN SELECT RAISE(ABORT, 'disk I/O error'); END"
    ))
    .execute(world.db())
    .await
    .unwrap();
}

#[tokio::test]
async fn a_run_whose_log_cannot_be_written_stops_before_the_model_is_asked() {
    let (world, agent, main, _dir) = echoing_agent().await;
    refuse_writes(&world, None).await;

    let reply = visitor_says(&world, &agent, "hello").await;
    assert_eq!(reply.status, chat::TurnStatus::Errored);
    assert_eq!(
        reply.error.as_deref(),
        Some(crate::agents::audit::LOG_UNAVAILABLE)
    );
    assert!(
        requests(&main).await.is_empty(),
        "nothing happened that the log could not show"
    );
}

#[tokio::test]
async fn a_lost_tool_result_stops_the_run_before_its_next_round() {
    let (world, agent, main, _dir) = echoing_agent().await;
    refuse_writes(&world, Some("tool_result")).await;

    let reply = visitor_says(&world, &agent, "hello").await;
    assert_eq!(reply.status, chat::TurnStatus::Errored);
    assert_eq!(
        reply.error.as_deref(),
        Some(crate::agents::audit::LOG_UNAVAILABLE)
    );
    assert_eq!(
        requests(&main).await.len(),
        1,
        "the round after the unrecorded result never went out"
    );
}
