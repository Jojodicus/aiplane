// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! `POST /api/v0/feedback/extract` — a spoken note turned into the report's
//! fields by one side call on the operator's chat model, under the
//! reporter's spend limits.

use crate::common;

use std::sync::Arc;

use aiplane::rama_server::router::router;
use aiplane_core::server::db::limits::{self, Dimension, SubjectType, Window};
use common::Service as _;
use rama::http::StatusCode;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const EXTRACT: &str = "/api/v0/feedback/extract";

async fn upstream() -> MockServer {
    let server = MockServer::start().await;
    let fields = json!({
        "title": "Export the usage table",
        "description": "The usage page has no export.",
        "business_value": "Finance needs it monthly.",
        "acceptance_criteria": "- a CSV download",
        "priority": "urgent",
    });
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": {
                "role": "assistant",
                "content": format!("```json\n{fields}\n```"),
            } }],
        })))
        .mount(&server)
        .await;
    server
}

async fn extract(
    state: &Arc<aiplane::rama_server::RamaState>,
    cookie: &str,
) -> (StatusCode, Value) {
    let resp = router(state.clone())
        .serve(common::post_json(
            EXTRACT,
            cookie,
            r#"{"transcript":"we need a usage export","locale":"de"}"#,
        ))
        .await
        .unwrap();
    let status = resp.status();
    let body = serde_json::from_slice(&common::read_body(resp).await).unwrap();
    (status, body)
}

#[tokio::test]
async fn a_transcript_becomes_the_reports_fields_through_one_side_call() {
    let server = upstream().await;
    let state = Arc::new(common::state_with_chat_pool(&server.uri()).await);
    let cookie = common::seed_session(&state, "alice", "alice@example.com").await;

    let (status, body) = extract(&state, &cookie).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["title"], "Export the usage table");
    assert_eq!(
        body["priority"], "medium",
        "an unknown priority reads as medium"
    );
    let sent: Vec<Value> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["model"], "model-a");
    assert_eq!(sent[0]["max_tokens"], 1200);
    assert_eq!(sent[0]["chat_template_kwargs"]["enable_thinking"], false);
    assert_eq!(
        sent[0]["response_format"]["json_schema"]["name"],
        "feedback_fields"
    );
    let input = sent[0]["messages"][1]["content"].as_str().unwrap();
    assert!(
        input.starts_with("Transcript:\nwe need a usage export"),
        "{input}"
    );
    assert!(input.contains("\"de\""), "{input}");
    assert!(input.ends_with("\n\n/no_think"), "{input}");
}

#[tokio::test]
async fn a_reporter_over_their_limit_is_refused_before_the_model_is_called() {
    let server = upstream().await;
    let state = Arc::new(common::state_with_chat_pool(&server.uri()).await);
    let cookie = common::seed_session(&state, "alice", "alice@example.com").await;
    limits::upsert(
        &state.db,
        SubjectType::User,
        "alice",
        None,
        Dimension::Requests,
        Window::Hour,
        0.0,
    )
    .await
    .unwrap();

    let (status, body) = extract(&state, &cookie).await;

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("limit reached"),
        "{body}"
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}
