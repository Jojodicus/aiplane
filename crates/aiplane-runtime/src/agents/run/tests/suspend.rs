// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Agent runs that pause for a decision and are resumed: a secure input the
//! visitor types, an approval staff give inside a sub-agent run, a request
//! nobody answers, and a pause that outlives the process.

use std::time::Duration;

use sqlx::Row;

use super::*;
use crate::agents::resume::{
    AgentResume, AgentResumeError, ResumedBy, claim, resume_expired, run_claimed,
};
use crate::server::tools::ToolRegistry;
use crate::server::tools::ask_first::AskFirst;
use crate::server::tools::check_code::{CHECK_CODE, CheckCode};
use crate::server::tools::echo::Echo;
use crate::server::tools::time::CurrentTimestamp;
use aiplane_agents::db::run_sessions;
use session_core::db::{Decision, DecisionKind, SuspensionKind};

const CODE: &str = "481516";
const ECHO: &str = "company_echo";

fn gated(timeout: Duration) -> ToolRegistry {
    ToolRegistry::new()
        .with(CurrentTimestamp)
        .with(AskFirst::new(Echo, timeout))
        .with(CheckCode::new(CODE, timeout))
}

async fn world(pools: &[(&str, &str, &MockServer)], timeout: Duration) -> World {
    World::build(pools, None, false, gated(timeout), None).await
}

fn main_spec(tools: &[&str]) -> Value {
    json!({ "main": {
        "model": "support-model",
        "instructions": { "orchestration": "Help the visitor." },
        "tools": tools,
        "budget": { "rounds": 6 }
    } })
}

async fn support(world: &World, tools: &[&str]) -> String {
    let mut grants = vec![(GrantKind::Model, "support-model")];
    grants.extend(tools.iter().map(|t| (GrantKind::Tool, *t)));
    let id = world.agent("support", &grants).await;
    world.publish(&id, &main_spec(tools)).await;
    id
}

pub(super) async fn visitor_says(world: &World, agent: &str, message: &str) -> AgentReply {
    run_turn(
        &world.state,
        AgentTurn {
            agent_id: agent,
            session_id: None,
            message,
            visitor_id: None,
            lang: None,
        },
    )
    .await
    .unwrap()
}

pub(super) fn answer(reply: &AgentReply, decision: Decision, by: ResumedBy) -> AgentResume<'_> {
    AgentResume {
        agent_id: "",
        session_id: &reply.session_id,
        turn_id: &reply.turn_id,
        request_id: reply.suspension.as_ref().map(|s| s.request_id.as_str()),
        decision,
        by,
    }
}

pub(super) fn staff() -> ResumedBy {
    ResumedBy::Staff {
        user_id: "u1".into(),
    }
}

/// Every text value in every table of the database, FTS shadow tables
/// included: where a value would be if anything had stored it.
pub(super) async fn every_stored_text(db: &aiplane_core::server::db::Pool) -> String {
    let tables: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table'")
            .fetch_all(db)
            .await
            .unwrap();
    let mut all = String::new();
    for table in tables {
        let rows = sqlx::query(&format!("SELECT * FROM \"{table}\""))
            .fetch_all(db)
            .await
            .unwrap_or_default();
        for row in rows {
            for i in 0..row.columns().len() {
                if let Ok(Some(text)) = row.try_get::<Option<String>, _>(i) {
                    all.push_str(&text);
                    all.push('\n');
                }
            }
        }
    }
    all
}

pub(super) async fn wait_settled(world: &World, session: &str, turn: &str) -> chat::Turn {
    for _ in 0..400 {
        let t = chat::get_turn(world.db(), session, turn)
            .await
            .unwrap()
            .unwrap();
        if t.status.is_terminal() {
            return t;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("turn {turn} never settled");
}

#[tokio::test]
async fn a_secure_input_reaches_the_tool_and_nothing_else() {
    let main = llm(vec![
        call("k1", CHECK_CODE, json!({})),
        text("Thanks, you are verified."),
    ])
    .await;
    let world = world(
        &[("support-pool", "support-model", &main)],
        Duration::from_secs(3600),
    )
    .await;
    let agent = support(&world, &[CHECK_CODE]).await;

    let paused = visitor_says(&world, &agent, "It's me, Alice.").await;
    assert_eq!(paused.status, chat::TurnStatus::Suspended);
    assert_eq!(paused.answer, None);
    let waiting = paused.suspension.clone().expect("what the turn waits for");
    assert_eq!(waiting.kind, SuspensionKind::SecureInput);
    assert_eq!(
        waiting.message.as_deref(),
        Some("Enter the code we sent you.")
    );
    assert_eq!(waiting.options, [DecisionKind::Value, DecisionKind::Deny]);
    assert_eq!(requests(&main).await.len(), 1, "the worker is gone");

    let resume = || AgentResume {
        agent_id: &agent,
        ..answer(
            &paused,
            Decision::Value { value: json!(CODE) },
            ResumedBy::Participant,
        )
    };
    let by_staff = claim(
        &world.state,
        AgentResume {
            by: staff(),
            ..resume()
        },
    )
    .await;
    assert!(
        matches!(by_staff, Err(AgentResumeError::ParticipantOnly { .. })),
        "{by_staff:?}"
    );

    let claimed = claim(&world.state, resume()).await.unwrap();
    let done = run_claimed(&world.state, claimed, RunOptions::default())
        .await
        .unwrap();
    assert_eq!(done.status, chat::TurnStatus::Completed);
    assert_eq!(done.answer.as_deref(), Some("Thanks, you are verified."));
    assert_eq!(done.turn_id, paused.turn_id, "the same turn continued");

    let sent = requests(&main).await;
    assert_eq!(sent.len(), 2);
    let checked = tool_answer(&sent, "k1");
    assert!(checked.contains("\"verified\": true"), "{checked}");
    for request in &sent {
        assert!(
            !request.to_string().contains(CODE),
            "the code never reaches the model"
        );
    }
    assert!(
        !every_stored_text(world.db()).await.contains(CODE),
        "the code is nowhere in the database: transcript, tool rows, audit"
    );
    let kinds: Vec<String> = world
        .audit(&agent)
        .await
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert!(kinds.contains(&"run_suspended".to_string()), "{kinds:?}");
    let resumed = world
        .audit(&agent)
        .await
        .into_iter()
        .find(|e| e.kind == "run_resumed")
        .expect("the resume is audited");
    assert_eq!(resumed.detail["answered_by"], "participant");
    assert_eq!(resumed.detail["decision"], "value");
    assert_eq!(resumed.detail["kind"], "secure_input");
}

fn billing_spec_with_echo() -> Value {
    json!({
        "main": {
            "model": "billing-model",
            "instructions": { "orchestration": "Issue the refund." },
            "tools": [ECHO],
            "budget": { "rounds": 4 }
        },
        "finish": { "schema": { "type": "object", "required": ["answer"],
                                "properties": { "answer": { "type": "string" } } } }
    })
}

fn refunds_spec(billing: &str) -> Value {
    json!({
        "main": {
            "model": "support-model",
            "instructions": { "orchestration": "Find out what the visitor needs, then forward it." },
            "budget": { "rounds": 6 }
        },
        "state": {
            "issue": { "type": "enum", "values": ["refund"], "set_by": ["llm"] }
        },
        "router": { "kind": "rules" },
        "routes": {
            "refunds": {
                "when": { "slot": "issue", "eq": "refund" },
                "agent": billing,
                "task": "Refund invoice RE-1."
            }
        }
    })
}

#[tokio::test]
async fn a_paused_sub_agent_pauses_its_caller_and_one_staff_decision_resumes_both() {
    let main = llm(vec![
        call("s1", "set_issue", json!({"value": "refund"})),
        call("fwd", "forward_request", json!({})),
        text("Your refund is on its way."),
    ])
    .await;
    let sub = llm(vec![
        call("e1", ECHO, json!({"message": "refund RE-1"})),
        finish("f1", json!({"answer": "Refund RE-1 issued."})),
    ])
    .await;
    let world = world(
        &[
            ("support-pool", "support-model", &main),
            ("billing-pool", "billing-model", &sub),
        ],
        Duration::from_secs(3600),
    )
    .await;
    let billing = world
        .agent(
            "billing",
            &[(GrantKind::Model, "billing-model"), (GrantKind::Tool, ECHO)],
        )
        .await;
    world.publish(&billing, &billing_spec_with_echo()).await;
    let support = world
        .agent("support", &[(GrantKind::Model, "support-model")])
        .await;
    assert_eq!(world.issues(&support, &refunds_spec(&billing)).await, []);
    world.publish(&support, &refunds_spec(&billing)).await;

    let paused = visitor_says(&world, &support, "Please refund RE-1.").await;
    assert_eq!(paused.status, chat::TurnStatus::Suspended);
    let waiting = paused.suspension.clone().unwrap();
    assert_eq!(waiting.kind, SuspensionKind::Approval);
    assert_eq!(waiting.tool.as_deref(), Some("forward_request"));
    let top = chat::get_suspension(world.db(), &paused.turn_id)
        .await
        .unwrap()
        .unwrap();
    let child_turn = top
        .child_turn
        .clone()
        .expect("the parent points at the child");
    let child = chat::get_suspension(world.db(), &child_turn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(child.kind, SuspensionKind::Approval);
    assert_eq!(child.tool_call.name, ECHO);
    assert_eq!(
        child.expires_at, top.expires_at,
        "they wait exactly as long"
    );
    assert_eq!(
        child.run_context,
        Some(json!({"route": "refunds", "route_binds": {}}))
    );
    let child_session = run_sessions::run_session_of_turn(world.db(), &child_turn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        child_session.parent_turn_id.as_deref(),
        Some(paused.turn_id.as_str())
    );
    assert_eq!(requests(&sub).await.len(), 1, "the echo has not run");

    let resume = |by| AgentResume {
        agent_id: &support,
        ..answer(&paused, Decision::AllowOnce, by)
    };
    let by_visitor = claim(&world.state, resume(ResumedBy::Participant)).await;
    assert!(
        matches!(by_visitor, Err(AgentResumeError::StaffOnly { .. })),
        "{by_visitor:?}"
    );
    assert!(
        chat::get_suspension(world.db(), &child_turn)
            .await
            .unwrap()
            .is_some(),
        "a refused answer claims nothing"
    );

    let claimed = claim(&world.state, resume(staff())).await.unwrap();
    let done = run_claimed(&world.state, claimed, RunOptions::default())
        .await
        .unwrap();
    assert_eq!(done.status, chat::TurnStatus::Completed);
    assert_eq!(done.answer.as_deref(), Some("Your refund is on its way."));

    let sub_sent = requests(&sub).await;
    assert_eq!(sub_sent.len(), 2);
    assert!(
        tool_answer(&sub_sent, "e1").contains("refund RE-1"),
        "the approved echo ran in the sub-agent"
    );
    let main_sent = requests(&main).await;
    let forwarded = tool_answer(&main_sent, "fwd");
    assert!(forwarded.contains("\"forwarded\": true"), "{forwarded}");
    assert!(forwarded.contains("Refund RE-1 issued."), "{forwarded}");
    let child_done = chat::get_turn(world.db(), &child_session.id, &child_turn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(child_done.status, chat::TurnStatus::Completed);
    for turn in [&paused.turn_id, &child_turn] {
        assert_eq!(chat::get_suspension(world.db(), turn).await.unwrap(), None);
    }

    let support_audit = world.audit(&support).await;
    let finished = support_audit
        .iter()
        .find(|e| e.kind == "sub_agent_finished")
        .expect("the sub-agent's outcome is audited on its caller");
    assert_eq!(finished.detail["route"], "refunds");
    assert_eq!(finished.detail["outcome"]["status"], "finished");
    let resumed = support_audit
        .iter()
        .find(|e| e.kind == "run_resumed")
        .unwrap();
    assert_eq!(resumed.actor_id.as_deref(), Some("u1"));
    assert_eq!(resumed.detail["waiting_turn"], child_turn.as_str());
    let billing_audit = world.audit(&billing).await;
    let echo_call = billing_audit
        .iter()
        .rfind(|e| e.kind == "tool_call" && e.detail["tool"] == ECHO)
        .unwrap();
    let frames = echo_call.chain.as_ref().unwrap()["frames"]
        .as_array()
        .unwrap();
    assert_eq!(frames.len(), 2, "the resumed child runs under its chain");
}

#[tokio::test]
async fn an_approval_nobody_gives_in_time_is_denied_and_the_tool_never_runs() {
    let main = llm(vec![
        call("e1", ECHO, json!({"message": "ship it"})),
        text("Nobody approved it in time."),
    ])
    .await;
    let world = world(&[("support-pool", "support-model", &main)], Duration::ZERO).await;
    let agent = support(&world, &[ECHO]).await;

    let paused = visitor_says(&world, &agent, "Ship it.").await;
    assert_eq!(paused.status, chat::TurnStatus::Suspended);
    resume_expired(&world.state).await;
    let done = wait_settled(&world, &paused.session_id, &paused.turn_id).await;
    assert_eq!(done.status, chat::TurnStatus::Completed);
    assert_eq!(done.content.as_deref(), Some("Nobody approved it in time."));

    let sent = requests(&main).await;
    let denied = tool_answer(&sent, "e1");
    assert!(denied.contains("expired"), "{denied}");
    assert!(!denied.contains("ship it"), "the echo never ran: {denied}");
    let resumed = world
        .audit(&agent)
        .await
        .into_iter()
        .find(|e| e.kind == "run_resumed")
        .unwrap();
    assert_eq!(resumed.detail["answered_by"], "timeout");
    assert_eq!(resumed.detail["decision"], "deny");
}

#[tokio::test]
async fn a_paused_agent_run_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("gateway.db");
    let main = llm(vec![
        call("e1", ECHO, json!({"message": "ship it"})),
        text("Shipped."),
    ])
    .await;
    let pools = [("support-pool", "support-model", &main)];
    let timeout = Duration::from_secs(3600);

    let before = World::build(&pools, None, false, gated(timeout), Some(&db_path)).await;
    let agent = support(&before, &[ECHO]).await;
    let paused = visitor_says(&before, &agent, "Ship it.").await;
    assert_eq!(paused.status, chat::TurnStatus::Suspended);
    before.db().close().await;
    drop(before);

    let after = World::build(&pools, None, false, gated(timeout), Some(&db_path)).await;
    let claimed = claim(
        &after.state,
        AgentResume {
            agent_id: &agent,
            ..answer(&paused, Decision::AllowOnce, staff())
        },
    )
    .await
    .unwrap();
    let done = run_claimed(&after.state, claimed, RunOptions::default())
        .await
        .unwrap();
    assert_eq!(done.status, chat::TurnStatus::Completed);
    assert_eq!(done.answer.as_deref(), Some("Shipped."));
    assert!(tool_answer(&requests(&main).await, "e1").contains("ship it"));
}

#[tokio::test]
async fn a_new_message_waits_while_the_conversation_waits_for_a_decision() {
    let main = llm(vec![call("e1", ECHO, json!({"message": "x"}))]).await;
    let world = world(
        &[("support-pool", "support-model", &main)],
        Duration::from_secs(3600),
    )
    .await;
    let agent = support(&world, &[ECHO]).await;
    let paused = visitor_says(&world, &agent, "Ship it.").await;

    let refused = run_turn(
        &world.state,
        AgentTurn {
            agent_id: &agent,
            session_id: Some(&paused.session_id),
            message: "and then?",
            visitor_id: None,
            lang: None,
        },
    )
    .await;
    assert!(
        matches!(refused, Err(AgentRunError::DecisionPending { ref turn, .. }) if *turn == paused.turn_id),
        "{refused:?}"
    );
}
