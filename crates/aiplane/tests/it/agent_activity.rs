// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! `GET /api/v0/agents/{id}/activity`, `…/activity/export` and
//! `…/activity/verify` (#111, `docs/agents.md` → "What #111 built"): the
//! activity log page by page with its filters, the JSONL export, the chain
//! check, and who may read it — admins and share holders, never a
//! responder.

use aiplane_agents::db::agent_audit::{self, AuditKind, Correlation, NewEvent};
use aiplane_core::server::principal::{GrantSet, SystemPrincipal};
use aiplane_core::server::run_chain::{Frame, RunChain};
use rama::http::{Body, Method, Request, StatusCode};
use serde_json::{Value, json};

use crate::agents::{Fx, fixture};
use crate::common;
use common::Service as _;

const VISITOR: &str = "Where is my parcel?";

async fn conversation_events(fx: &Fx, agent: &str, conversation: &str) {
    let principal = SystemPrincipal {
        id: agent.into(),
        name: "support".into(),
        grants: std::sync::Arc::new(GrantSet::default()),
    };
    let chain = RunChain::root(
        conversation,
        Some("v-1".into()),
        Frame::for_principal(&principal, Some(1)),
    );
    let events = [
        (AuditKind::TurnStarted, json!({ "message": VISITOR })),
        (
            AuditKind::LlmExchange,
            json!({ "request": { "messages": [VISITOR] }, "response": { "content": "On its way." } }),
        ),
        (
            AuditKind::TurnFinished,
            json!({ "status": "completed", "answer": "On its way." }),
        ),
    ];
    for (kind, detail) in events {
        agent_audit::append_now(
            &fx.state.db,
            NewEvent::new(kind, agent, detail)
                .in_run(Some(&chain))
                .at(Correlation {
                    session_id: Some(conversation.into()),
                    turn_id: Some(format!("{conversation}-t1")),
                    round: Some(0),
                    ..Correlation::default()
                }),
        )
        .await
        .unwrap();
    }
}

async fn setup() -> (Fx, String) {
    let fx = fixture().await;
    let agent = fx.create(&fx.alice, "support").await;
    conversation_events(&fx, &agent, "s-1").await;
    conversation_events(&fx, &agent, "s-2").await;
    let other = fx.create(&fx.alice, "other").await;
    conversation_events(&fx, &other, "s-other").await;
    (fx, agent)
}

async fn activity(fx: &Fx, cookie: &str, agent: &str, query: &str) -> (StatusCode, Value) {
    fx.get(cookie, &format!("/api/v0/agents/{agent}/activity?{query}"))
        .await
}

fn kinds(body: &Value) -> Vec<&str> {
    body["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn a_conversation_reads_as_its_timeline_with_every_correlation_id() {
    let (fx, agent) = setup().await;
    let (status, body) = activity(&fx, &fx.alice, &agent, "conversation=s-1&order=asc").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        kinds(&body),
        ["turn_started", "llm_exchange", "turn_finished"]
    );
    let first = &body["events"][0];
    assert_eq!(first["detail"]["message"], VISITOR);
    assert_eq!(first["conversation_id"], "s-1");
    assert_eq!(first["turn_id"], "s-1-t1");
    assert_eq!(first["visitor_id"], "v-1");
    assert_eq!(first["version"], 1);
    assert_eq!(first["seq"], 1);
    assert_eq!(first["chain_key"], "conversation:s-1");
    assert_eq!(body["events"][1]["prev_hash"], first["hash"]);
    assert_eq!(body["next_cursor"], Value::Null);
}

#[tokio::test]
async fn the_agents_log_pages_by_cursor_and_filters_by_kind_and_time() {
    let (fx, agent) = setup().await;
    let (_, all) = activity(&fx, &fx.alice, &agent, "").await;
    let total = all["events"].as_array().unwrap().len();
    assert_eq!(
        total, 8,
        "both conversations and creating the agent and its principal, nothing of the other agent"
    );
    assert_eq!(all["events"][0]["kind"], "turn_finished", "newest first");

    let mut seen = Vec::new();
    let mut cursor = String::new();
    loop {
        let (status, page) =
            activity(&fx, &fx.alice, &agent, &format!("limit=3&cursor={cursor}")).await;
        assert_eq!(status, StatusCode::OK);
        seen.extend(
            page["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["id"].clone()),
        );
        match page["next_cursor"].as_i64() {
            Some(next) => cursor = next.to_string(),
            None => break,
        }
    }
    let ids: Vec<Value> = all["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].clone())
        .collect();
    assert_eq!(seen, ids);

    let (_, exchanges) = activity(&fx, &fx.alice, &agent, "kind=llm_exchange,turn_started").await;
    assert_eq!(exchanges["events"].as_array().unwrap().len(), 4);
    let (_, future) = activity(&fx, &fx.alice, &agent, "from=2999-01-01").await;
    assert!(future["events"].as_array().unwrap().is_empty());

    for query in [
        "kind=everything",
        "order=sideways",
        "limit=0",
        "cursor=x",
        "from=yesterday",
    ] {
        let (status, body) = activity(&fx, &fx.alice, &agent, query).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {body}");
    }
}

#[tokio::test]
async fn the_export_streams_every_event_as_one_json_line_oldest_first() {
    let (fx, agent) = setup().await;
    let req = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/api/v0/agents/{agent}/activity/export?conversation=s-2"
        ))
        .header("cookie", format!("id={}", fx.alice))
        .body(Body::empty())
        .unwrap();
    let resp = common::app(fx.state.clone()).serve(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/x-ndjson"
    );
    assert!(
        resp.headers()
            .get("content-disposition")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("agent-support-activity.jsonl")
    );
    let body = String::from_utf8(common::read_body(resp).await.to_vec()).unwrap();
    let lines: Vec<Value> = body
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let seqs: Vec<i64> = lines.iter().map(|l| l["seq"].as_i64().unwrap()).collect();
    assert_eq!(seqs, [1, 2, 3]);
    assert!(lines.iter().all(|l| l["conversation_id"] == "s-2"));
}

#[tokio::test]
async fn verify_reports_an_intact_log_and_then_the_link_that_was_changed() {
    let (fx, agent) = setup().await;
    let uri = format!("/api/v0/agents/{agent}/activity/verify");
    let (status, body) = fx.get(&fx.alice, &uri).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true);
    assert_eq!(body["chains"], 3, "two conversations and the agent's own");
    assert_eq!(body["head"]["chain_key"], format!("agent:{agent}"));
    assert_eq!(body["head"]["seq"], 2);
    assert_eq!(
        body["unanchored"], 6,
        "no turn ended, so nothing is anchored"
    );
    assert_eq!(body["events"], 8);
    assert_eq!(body["checked"], 8);

    sqlx::query(
        "UPDATE agent_audit SET detail = '{\"message\":\"something else\"}'
          WHERE conversation_id = 's-2' AND seq = 1",
    )
    .execute(&fx.state.db)
    .await
    .unwrap();
    let (_, body) = fx.get(&fx.alice, &uri).await;
    assert_eq!(body["ok"], true, "{body}");
    assert_eq!(
        (body["events"].clone(), body["checked"].clone()),
        (json!(8), json!(0)),
        "a check resumes where the last one left each chain"
    );

    let (_, body) = fx.get(&fx.alice, &format!("{uri}?full=true")).await;
    assert_eq!(body["ok"], false);
    assert_eq!(body["broken"]["chain_key"], "conversation:s-2");
    assert_eq!(body["broken"]["seq"], 1);

    let (status, body) = fx.get(&fx.alice, &format!("{uri}?full=maybe")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn admins_and_share_holders_read_the_log_and_responders_do_not() {
    let (fx, agent) = setup().await;
    let routes = [
        format!("/api/v0/agents/{agent}/activity"),
        format!("/api/v0/agents/{agent}/activity/export"),
        format!("/api/v0/agents/{agent}/activity/verify"),
    ];
    let (status, body) = fx
        .post(
            &fx.alice,
            &format!("/api/v0/agents/{agent}/responders"),
            json!({ "subject_kind": "user", "subject_id": "plain" }),
        )
        .await;
    assert!(status.is_success(), "{body}");
    for uri in &routes {
        assert_eq!(
            fx.get(&fx.plain, uri).await.0,
            StatusCode::FORBIDDEN,
            "responder: {uri}"
        );
        assert_eq!(
            fx.get(&fx.bob, uri).await.0,
            StatusCode::NOT_FOUND,
            "no share: {uri}"
        );
        assert_eq!(
            fx.get(&fx.root, uri).await.0,
            StatusCode::OK,
            "admin: {uri}"
        );
        assert_ne!(
            fx.send(None, Method::GET, uri, None).await.0,
            StatusCode::OK
        );
    }
    let shared = fx.share(&fx.alice, &agent, "user", "bob", "read").await;
    assert!(shared.0.is_success(), "{shared:?}");
    for uri in &routes {
        assert_eq!(
            fx.get(&fx.bob, uri).await.0,
            StatusCode::OK,
            "read share: {uri}"
        );
    }
}

/// A round stored as a delta, with an image stored as a blob, reads back as
/// the whole request it stood for, on a page and in the export.
#[tokio::test]
async fn a_delta_and_its_blobs_read_back_as_the_whole_request() {
    let (fx, agent) = setup().await;
    let principal = SystemPrincipal {
        id: agent.clone(),
        name: "support".into(),
        grants: std::sync::Arc::new(GrantSet::default()),
    };
    let chain = RunChain::root("s-d", None, Frame::for_principal(&principal, Some(1)));
    let image = format!(
        "data:image/png;base64,{}",
        "Q".repeat(agent_audit::exchange::BLOB_MIN_BYTES)
    );
    let first = json!({ "model": "m", "messages": [
        { "role": "system", "content": "s" },
        { "role": "user", "content": [{ "type": "image_url", "image_url": { "url": image } }] }
    ] });
    let mut second = first.clone();
    second["messages"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "role": "assistant", "content": "a cat" }));
    let append = |detail: Value| {
        agent_audit::append_now(
            &fx.state.db,
            NewEvent::new(AuditKind::LlmExchange, &agent, detail).in_run(Some(&chain)),
        )
    };
    let a = append(json!({ "purpose": "round", "request": first }))
        .await
        .unwrap();
    let mut delta = agent_audit::exchange::request_delta(&first, &second).unwrap();
    delta["prev"] = json!(a.id);
    append(json!({ "purpose": "round", "request_delta": delta }))
        .await
        .unwrap();

    let (status, page) = activity(&fx, &fx.alice, &agent, "conversation=s-d&order=asc").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let requests: Vec<Value> = page["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["detail"]["request"].clone())
        .collect();
    assert_eq!(requests, [first.clone(), second.clone()]);

    let req = Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/api/v0/agents/{agent}/activity/export?conversation=s-d"
        ))
        .header("cookie", format!("id={}", fx.alice))
        .body(Body::empty())
        .unwrap();
    let resp = common::app(fx.state.clone()).serve(req).await.unwrap();
    let body = String::from_utf8(common::read_body(resp).await.to_vec()).unwrap();
    let exported: Vec<Value> = body
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap()["detail"]["request"].clone())
        .collect();
    assert_eq!(exported, [first, second]);
}
