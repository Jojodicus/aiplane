// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Human in the loop over HTTP (`docs/agents.md` "What #96 built"): a
//! handoff a responder answers from the inbox and the visitor receives, what
//! a responder can and cannot reach, someone who may not answer, the
//! responder and channel management routes, and a scheduled run that paused
//! and is resumed from its owner's inbox.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rama::Service;
use rama::http::{Body, Method, Request, StatusCode, header};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{Embed, SITE, frames, new_key};
use crate::agents::{self, Fx, TIME, person};
use crate::common;

use aiplane_core::server::db::gateway_groups;
use aiplane_runtime::agents::embed::LiveAgentRunner;
use aiplane_runtime::server::scheduled::{self, NewAction};
use aiplane_runtime::server::tools::ToolRegistry;
use aiplane_runtime::server::tools::ask_first::AskFirst;
use aiplane_runtime::server::tools::echo::Echo;
use aiplane_runtime::server::tools::time::CurrentTimestamp;
use session_core::db as chat;

const ECHO: &str = "company_echo";
const QUESTION: &str = "May we refund invoice RE-1, which was billed twice?";
const RELAYED: &str = "Good news: our team approved your refund.";

struct Scripted {
    deltas: Vec<Value>,
    served: AtomicUsize,
}

impl wiremock::Respond for Scripted {
    fn respond(&self, _req: &wiremock::Request) -> ResponseTemplate {
        let i = self.served.fetch_add(1, Ordering::SeqCst);
        let delta = &self.deltas[i.min(self.deltas.len() - 1)];
        let sse = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"choices": [{"index": 0, "delta": delta}]})
        );
        ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream")
    }
}

async fn upstream(deltas: Vec<Value>) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(Scripted {
            deltas,
            served: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    server
}

fn call(id: &str, name: &str, args: Value) -> Value {
    json!({"tool_calls": [{"index": 0, "id": id, "type": "function",
        "function": {"name": name, "arguments": args.to_string()}}]})
}

fn text(s: &str) -> Value {
    json!({ "content": s })
}

fn handoff_spec() -> Value {
    json!({
        "main": {
            "pool": "pool",
            "instructions": { "orchestration": "Hand refunds to a person." },
            "tools": [TIME]
        },
        "state": { "issue": { "type": "string", "max_length": 200, "set_by": ["llm"] } },
        "routes": { "people": {
            "when": { "slot": "issue", "set": true },
            "human": { "inbox": "billing desk" }
        } }
    })
}

/// Sam answers for the `support` group; Nora holds nothing.
struct Staff {
    sam: String,
    nora: String,
}

async fn staff(fx: &Fx) -> Staff {
    gateway_groups::set_mappings_for_group(&fx.state.db, "support", &["support".into()])
        .await
        .unwrap();
    fx.state.reload_rbac().await;
    Staff {
        sam: person(&fx.state, "sam", &["support"]).await,
        nora: person(&fx.state, "nora", &[]).await,
    }
}

/// A published agent with a human route, served by the production runner on
/// `llm`, and one embed key.
async fn handing_off(llm: &MockServer) -> Embed {
    handing_off_with(llm, handoff_spec()).await
}

async fn handing_off_with(llm: &MockServer, spec: Value) -> Embed {
    let mut fx = agents::fixture_on(Some(&llm.uri())).await;
    fx.state = fx
        .state
        .clone()
        .with_agent_runner(Arc::new(LiveAgentRunner));
    let agent = fx.runnable("support").await;
    let (status, body) = fx.put_draft(&fx.alice, &agent, spec).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = fx.publish(&fx.alice, &agent).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (key_id, key) = new_key(&fx, &agent, &[SITE]).await;
    Embed {
        fx,
        agent,
        key_id,
        key,
        runner: Arc::default(),
    }
}

impl Embed {
    async fn quiet(&self, token: &str) -> chat::Turn {
        let session = self.conversation_of(token).await;
        for _ in 0..500 {
            let turns = chat::list_turns(&self.fx.state.db, &session).await.unwrap();
            let last = turns.last().unwrap().turn.clone();
            let running = last.status == chat::TurnStatus::InProgress
                || self.fx.state.chats.get(&self.agent, &session).is_some();
            if !running && last.role == chat::TurnRole::Assistant {
                return last;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the conversation never settled");
    }

    async fn stream(&self, token: &str) -> Vec<(String, Value)> {
        let resp = self
            .raw(
                Method::GET,
                "/api/v0/embed/events",
                Some(SITE),
                Some(token),
                None,
            )
            .await;
        let body = tokio::time::timeout(Duration::from_secs(20), common::read_body(resp))
            .await
            .expect("the stream ends");
        frames(&body)
    }
}

#[tokio::test]
async fn a_responder_answers_a_handoff_from_the_inbox_and_the_visitor_receives_it() {
    let llm = upstream(vec![
        call("s1", "set_issue", json!({"value": "refund for RE-1"})),
        call("h1", "request_human", json!({"question": QUESTION})),
        text(RELAYED),
    ])
    .await;
    let e = handing_off(&llm).await;
    let people = staff(&e.fx).await;
    let (status, body) =
        e.fx.post(
            &e.fx.alice,
            &format!("/api/v0/agents/{}/responders", e.agent),
            json!({ "subject_kind": "group", "subject_id": "support" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let token = e.visitor().await;
    assert_eq!(
        e.say(&token, "I was billed twice for RE-1.").await.status,
        StatusCode::ACCEPTED
    );
    let paused = e.quiet(&token).await;
    assert_eq!(paused.status, chat::TurnStatus::Suspended);
    let waiting = e.stream(&token).await;
    let (event, frame) = waiting.last().unwrap();
    assert_eq!(event, "suspended");
    assert_eq!(frame["kind"], "human_answer");
    assert_eq!(frame["options"], json!([]), "the visitor can only wait");

    let (status, inbox) = e.fx.get(&people.sam, "/api/v0/agents/inbox").await;
    assert_eq!(status, StatusCode::OK, "{inbox}");
    assert_eq!(inbox["count"], 1);
    let item = &inbox["items"][0];
    assert_eq!(item["standing"], "responder");
    assert_eq!(item["kind"], "human_answer");
    assert_eq!(item["question"], QUESTION);
    assert_eq!(item["agent"]["display"], "support");
    assert_eq!(
        item["context"]["visitor_message"],
        "I was billed twice for RE-1."
    );
    assert_eq!(item["context"]["inbox"], "billing desk");
    let id = item["id"].as_str().unwrap().to_string();

    let (status, body) =
        e.fx.post(
            &people.sam,
            &format!("/api/v0/agents/inbox/{id}/answer"),
            json!({ "decision": "value", "value": "Yes, refund it." }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let done = e.quiet(&token).await;
    assert_eq!(done.status, chat::TurnStatus::Completed);
    assert_eq!(done.content.as_deref(), Some(RELAYED));

    let session = e.resume(&token).await;
    let last = session.body["turns"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(last["turn"]["content"], RELAYED, "the visitor receives it");
    let (_, after) = e.fx.get(&people.sam, "/api/v0/agents/inbox").await;
    assert_eq!(after["count"], 0);

    let resumed = aiplane_agents::db::agent_audit::for_principal(&e.fx.state.db, &e.agent)
        .await
        .unwrap()
        .into_iter()
        .find(|ev| ev.kind == "run_resumed")
        .expect("the answer is audited");
    assert_eq!(resumed.actor_id.as_deref(), Some("sam"));
}

/// The test chat shows its manager a hand-off with what the person answering
/// would see in the Inbox, and keeps it out of the Inbox.
#[tokio::test]
async fn a_test_chat_handoff_carries_its_context_and_stays_out_of_the_inbox() {
    let llm = upstream(vec![
        call("s1", "set_issue", json!({"value": "refund for RE-1"})),
        call("h1", "request_human", json!({"question": QUESTION})),
    ])
    .await;
    let e = handing_off(&llm).await;
    let (status, paused) =
        e.fx.post(
            &e.fx.alice,
            &format!("/api/v0/agents/{}/test-turn", e.agent),
            json!({ "message": "I was billed twice for RE-1." }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{paused}");
    assert_eq!(paused["status"], "suspended");
    let suspension = &paused["suspension"];
    assert_eq!(suspension["kind"], "human_answer");
    assert_eq!(suspension["message"], QUESTION);
    let context = &suspension["context"];
    assert_eq!(context["visitor_message"], "I was billed twice for RE-1.");
    assert_eq!(
        context["slots"],
        json!([{ "slot": "issue", "value": "refund for RE-1" }])
    );
    assert_eq!(context["inbox"], "billing desk");

    let (status, inbox) = e.fx.get(&e.fx.alice, "/api/v0/agents/inbox").await;
    assert_eq!(status, StatusCode::OK, "{inbox}");
    assert_eq!(
        inbox["count"], 0,
        "a test-chat pause is answered in the test chat"
    );
}

#[tokio::test]
async fn a_german_responder_resumes_an_english_visitor_in_english() {
    let llm = upstream(vec![
        call("s1", "set_issue", json!({"value": "refund for RE-1"})),
        call("h1", "request_human", json!({"question": QUESTION})),
        text("RE-999 is refunded."),
    ])
    .await;
    let mut spec = handoff_spec();
    spec["publish"] = json!({ "output_filter": { "patterns": { "invoice": "RE-\\d+" } } });
    let e = handing_off_with(&llm, spec).await;
    let people = staff(&e.fx).await;
    let (status, body) =
        e.fx.post(
            &e.fx.alice,
            &format!("/api/v0/agents/{}/responders", e.agent),
            json!({ "subject_kind": "group", "subject_id": "support" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let token = e.visitor().await;
    let said = e.say_in(&token, "I was billed twice for RE-1.", "en").await;
    assert_eq!(said.status, StatusCode::ACCEPTED);
    assert_eq!(e.quiet(&token).await.status, chat::TurnStatus::Suspended);
    let (_, inbox) = e.fx.get(&people.sam, "/api/v0/agents/inbox").await;
    let id = inbox["items"][0]["id"].as_str().unwrap().to_string();

    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("/api/v0/agents/inbox/{id}/answer"))
        .header("cookie", format!("id={}", people.sam))
        .header(header::ACCEPT_LANGUAGE, "de")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "decision": "value", "value": "Yes, refund it." }).to_string(),
        ))
        .unwrap();
    let resp = common::app(e.fx.state.clone()).serve(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let done = e.quiet(&token).await;
    assert_eq!(
        done.content.as_deref(),
        Some(session_core::i18n::t(session_core::i18n::Lang::En, "agent-output-withheld").as_str())
    );
}

#[tokio::test]
async fn a_responder_sees_the_item_and_nothing_else_of_the_agent() {
    let llm = upstream(vec![
        call("s1", "set_issue", json!({"value": "refund"})),
        call("h1", "request_human", json!({"question": QUESTION})),
    ])
    .await;
    let e = handing_off(&llm).await;
    let people = staff(&e.fx).await;
    e.fx.post(
        &e.fx.alice,
        &format!("/api/v0/agents/{}/responders", e.agent),
        json!({ "subject_kind": "user", "subject_id": "sam" }),
    )
    .await;
    let token = e.visitor().await;
    e.say(&token, "Refund please.").await;
    e.quiet(&token).await;
    let session = e.conversation_of(&token).await;
    let turns = chat::list_turns(&e.fx.state.db, &session).await.unwrap();
    let turn = &turns.last().unwrap().turn.id;

    for uri in [
        "/api/v0/agents".to_string(),
        format!("/api/v0/agents/{}", e.agent),
        format!("/api/v0/agents/{}/versions", e.agent),
        format!("/api/v0/agents/{}/responders", e.agent),
        format!("/api/v0/agents/{}/channels", e.agent),
    ] {
        let (status, _) = e.fx.get(&people.sam, &uri).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
    }
    let (status, _) =
        e.fx.post(
            &people.sam,
            &format!(
                "/api/v0/agents/{}/conversations/{session}/turns/{turn}/resume",
                e.agent
            ),
            json!({ "decision": "value", "value": "x" }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "not through the staff route");

    let (_, inbox) = e.fx.get(&people.sam, "/api/v0/agents/inbox").await;
    let item = &inbox["items"][0];
    assert!(item.get("tail").is_none() && item["context"].get("transcript").is_none());
    let text = inbox.to_string();
    assert!(
        !text.contains("Hand refunds to a person"),
        "nothing of the spec: {text}"
    );
}

#[tokio::test]
async fn someone_who_may_not_answer_sees_nothing_and_cannot_answer() {
    let llm = upstream(vec![
        call("s1", "set_issue", json!({"value": "refund"})),
        call("h1", "request_human", json!({"question": QUESTION})),
        text(RELAYED),
    ])
    .await;
    let e = handing_off(&llm).await;
    let people = staff(&e.fx).await;
    let token = e.visitor().await;
    e.say(&token, "Refund please.").await;
    e.quiet(&token).await;

    let (_, alice) = e.fx.get(&e.fx.alice, "/api/v0/agents/inbox").await;
    assert_eq!(
        alice["items"][0]["standing"], "manager",
        "the writer sees it"
    );
    let id = alice["items"][0]["id"].as_str().unwrap().to_string();
    for who in [&people.nora, &people.sam, &e.fx.bob, &e.fx.plain] {
        let (_, inbox) = e.fx.get(who, "/api/v0/agents/inbox").await;
        assert_eq!(inbox["count"], 0, "{inbox}");
        let (status, body) =
            e.fx.post(
                who,
                &format!("/api/v0/agents/inbox/{id}/answer"),
                json!({ "decision": "value", "value": "sure" }),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["error"]["code"], "inbox_item_not_found");
    }
    assert_eq!(e.quiet(&token).await.status, chat::TurnStatus::Suspended);

    let (status, body) =
        e.fx.post(
            &e.fx.alice,
            &format!("/api/v0/agents/inbox/{id}/answer"),
            json!({ "decision": "allow_once" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "decision_not_offered");
}

#[tokio::test]
async fn responders_and_channels_are_managed_with_a_share_and_a_url_is_never_shown() {
    let fx = agents::fixture().await;
    staff(&fx).await;
    let agent = fx.runnable("support").await;
    let base = format!("/api/v0/agents/{agent}");

    let (status, _) = fx
        .post(
            &fx.alice,
            &format!("{base}/responders"),
            json!({ "subject_kind": "group", "subject_id": "nobody-has-this" }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = fx
        .post(
            &fx.bob,
            &format!("{base}/responders"),
            json!({ "subject_kind": "user", "subject_id": "nora" }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "bob holds no share");
    let (status, _) = fx
        .post(
            &fx.alice,
            &format!("{base}/responders"),
            json!({ "subject_kind": "user", "subject_id": "nora" }),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "a responder needs no manager permission"
    );
    let (_, listed) = fx.get(&fx.alice, &format!("{base}/responders")).await;
    assert_eq!(listed["responders"][0]["subject_id"], "nora");
    let (status, _) = fx
        .post(
            &fx.alice,
            &format!("{base}/responders/revoke"),
            json!({ "subject_kind": "user", "subject_id": "nora" }),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let secret = "https://hooks.slack.com/services/T0/B0/very-secret";
    let (status, body) = fx
        .post(
            &fx.alice,
            &format!("{base}/channels"),
            json!({ "kind": "slack", "name": "ops", "url": "https://evil.example/x" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "invalid_webhook_url");
    let (status, created) = fx
        .post(
            &fx.alice,
            &format!("{base}/channels"),
            json!({ "kind": "slack", "name": "ops", "url": secret, "details": true }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let (status, _) = fx
        .post(
            &fx.alice,
            &format!("{base}/channels"),
            json!({ "kind": "slack", "name": "ops", "url": secret }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, listed) = fx.get(&fx.alice, &format!("{base}/channels")).await;
    assert_eq!(listed["channels"][0]["url_host"], "hooks.slack.com");
    assert!(!listed.to_string().contains("very-secret"));
    assert!(!created.to_string().contains("very-secret"));
    let id = created["channel"]["id"].as_str().unwrap();
    let (status, _) = fx
        .send(
            Some(&fx.alice),
            Method::DELETE,
            &format!("{base}/channels/{id}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let kinds = fx.audit_kinds(&agent).await;
    for kind in [
        "responder_added",
        "responder_removed",
        "channel_created",
        "channel_deleted",
    ] {
        assert!(kinds.iter().any(|k| k == kind), "{kind} in {kinds:?}");
    }
}

#[tokio::test]
async fn a_scheduled_run_that_paused_is_resumed_from_its_owners_inbox() {
    let llm = upstream(vec![
        call("e1", ECHO, json!({"message": "nightly refunds"})),
        text("Refunds issued."),
    ])
    .await;
    let tools = ToolRegistry::new()
        .with(CurrentTimestamp)
        .with(AskFirst::new(Echo, Duration::from_secs(3600)));
    let fx = agents::fixture_with(Some(&llm.uri()), tools, &[TIME, ECHO]).await;
    let action = scheduled::create(
        &fx.state.db,
        NewAction {
            user_id: "plain".into(),
            name: "Nightly refunds".into(),
            prompt: "Issue the refunds that are due.".into(),
            model: "m".into(),
            cron: "0 3 * * *".into(),
            timezone: "UTC".into(),
            tools_enabled: true,
            reuse_conversation: false,
            reuse_rounds: 0,
            next_run_at: Some(jiff::Timestamp::now() - jiff::SignedDuration::from_secs(60)),
        },
    )
    .await
    .unwrap();
    let state = Arc::new(fx.state.clone());
    scheduled::worker::spawn(state);

    let mut item = Value::Null;
    for _ in 0..500 {
        let (_, inbox) = fx.get(&fx.plain, "/api/v0/agents/inbox").await;
        if inbox["count"] == 1 {
            item = inbox["items"][0].clone();
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(item["standing"], "owner", "{item}");
    assert_eq!(item["kind"], "approval");
    assert_eq!(item["title"], "Nightly refunds");
    assert_eq!(item["call"]["name"], ECHO);
    let (_, others) = fx.get(&fx.alice, "/api/v0/agents/inbox").await;
    assert_eq!(others["count"], 0, "nobody else's");
    let mut recorded = None;
    for _ in 0..500 {
        let runs = scheduled::list_runs(&fx.state.db, &action.id, 10)
            .await
            .unwrap();
        recorded = runs.first().and_then(|r| r.status.clone());
        if recorded.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(recorded.as_deref(), Some("waiting"));

    let id = item["id"].as_str().unwrap();
    let (status, body) = fx
        .post(
            &fx.plain,
            &format!("/api/v0/agents/inbox/{id}/answer"),
            json!({ "decision": "allow_once" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let session = item["session_id"].as_str().unwrap();
    let turn = item["turn_id"].as_str().unwrap();
    for _ in 0..500 {
        let t = chat::get_turn(&fx.state.db, session, turn)
            .await
            .unwrap()
            .unwrap();
        if t.status == chat::TurnStatus::Completed {
            assert_eq!(t.content.as_deref(), Some("Refunds issued."));
            let sent = llm.received_requests().await.unwrap();
            let last: Value = serde_json::from_slice(&sent.last().unwrap().body).unwrap();
            assert!(
                last.to_string().contains("nightly refunds"),
                "the approved call ran"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the resumed run never finished");
}
