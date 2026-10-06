// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! `/v1/responses` — the OpenAI Responses API that Codex CLI speaks.
//!
//! The unit tests in `rama_server::responses` pin the format mapping and the
//! store in isolation; these drive the routes against a wiremock upstream, so
//! they cover what only the wiring can get wrong: what the backend receives,
//! how a tool turn and a stored chain round-trip, what the stream looks like on
//! the wire, who may read a stored response, and whether routing, limit and
//! error decisions match `/v1/chat/completions`.

use crate::common;

use common::Service as _;
use rama::http::{Body, Method, Request, StatusCode};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn create_req(bearer: &str, body: &Value) -> Request {
    Request::builder()
        .method(Method::POST)
        .uri("/v1/responses")
        .header("authorization", format!("Bearer {bearer}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn bearer_req(method: Method, bearer: &str, uri: &str) -> Request {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {bearer}"))
        .body(Body::empty())
        .unwrap()
}

fn completion(content: &str) -> Value {
    json!({
        "id": "chatcmpl-x",
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "finish_reason": "stop",
            "message": {"role": "assistant", "content": content},
        }],
        "usage": {"prompt_tokens": 11, "completion_tokens": 5, "total_tokens": 16},
    })
}

async fn mount_chat(upstream: &MockServer, body: Value) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(upstream)
        .await;
}

async fn mount_stream(upstream: &MockServer, sse: &'static str) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .mount(upstream)
        .await;
}

/// Every request body the upstream received, in order.
async fn upstream_bodies(upstream: &MockServer) -> Vec<Value> {
    upstream
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| serde_json::from_slice(&r.body).expect("upstream body is JSON"))
        .collect()
}

async fn json_body(resp: rama::http::Response) -> Value {
    let bytes = common::read_body(resp).await;
    serde_json::from_slice(&bytes).expect("response body is JSON")
}

fn sse_events(body: &str) -> Vec<(String, Value)> {
    body.split("\n\n")
        .filter(|f| !f.trim().is_empty())
        .map(|frame| {
            let mut name = String::new();
            let mut data = Value::Null;
            for line in frame.lines() {
                if let Some(rest) = line.strip_prefix("event: ") {
                    name = rest.to_string();
                } else if let Some(rest) = line.strip_prefix("data: ") {
                    data = serde_json::from_str(rest).unwrap_or(Value::Null);
                }
            }
            (name, data)
        })
        .collect()
}

async fn stream_events<S>(app: &S, bearer: &str, body: &Value) -> Vec<(String, Value)>
where
    S: common::Service<Request, Output = rama::http::Response, Error = std::convert::Infallible>,
{
    let resp = app.serve(create_req(bearer, body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream")
    );
    let bytes = common::read_body(resp).await;
    sse_events(&String::from_utf8_lossy(&bytes))
}

// ---------------------------------------------------------------- auth

#[tokio::test]
async fn a_request_without_a_credential_is_401() {
    let state = common::state_with_chat_pool("http://unused.invalid").await;
    let app = common::app(state);
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/responses")
        .body(Body::from(
            json!({"model": "model-a", "input": "hi"}).to_string(),
        ))
        .unwrap();
    let resp = app.serve(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(json_body(resp).await["error"]["message"].is_string());
}

// ------------------------------------------------------- buffered turns

#[tokio::test]
async fn a_buffered_turn_is_translated_in_both_directions() {
    let upstream = MockServer::start().await;
    mount_chat(&upstream, completion("hello from the backend")).await;
    let state = common::state_with_chat_pool(&upstream.uri()).await;
    let bearer = common::seed_user_with_token(&state, "alice").await;
    let app = common::app(state);

    let body = json!({
        "model": "model-a",
        "instructions": "You are Codex.",
        "input": "hi",
        "max_output_tokens": 256,
        "store": false,
        "include": ["reasoning.encrypted_content"],
    });
    let resp = app.serve(create_req(&bearer, &body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let out = json_body(resp).await;
    assert_eq!(out["object"], "response");
    assert_eq!(out["status"], "completed");
    assert_eq!(out["model"], "model-a");
    assert_eq!(out["instructions"], "You are Codex.");
    assert_eq!(out["output"][0]["type"], "message");
    assert_eq!(
        out["output"][0]["content"][0]["text"],
        "hello from the backend"
    );
    assert_eq!(out["output_text"], "hello from the backend");
    assert_eq!(out["usage"]["input_tokens"], 11);
    assert_eq!(out["usage"]["output_tokens"], 5);
    assert_eq!(out["usage"]["total_tokens"], 16);

    let sent = &upstream_bodies(&upstream).await[0];
    assert_eq!(sent["max_tokens"], 256);
    assert_eq!(
        sent["messages"][0],
        json!({"role": "system", "content": "You are Codex."})
    );
    assert_eq!(
        sent["messages"][1],
        json!({"role": "user", "content": "hi"})
    );
    assert!(sent.get("include").is_none());
    assert!(sent.get("instructions").is_none());
}

#[tokio::test]
async fn a_client_function_call_round_trips() {
    let upstream = MockServer::start().await;
    mount_chat(
        &upstream,
        json!({
            "choices": [{"index": 0, "finish_reason": "tool_calls", "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{"id": "call_2", "type": "function",
                                "function": {"name": "shell", "arguments": "{\"cmd\":\"cat b.rs\"}"}}],
            }}],
        }),
    )
    .await;
    let state = common::state_with_chat_pool(&upstream.uri()).await;
    let bearer = common::seed_user_with_token(&state, "alice").await;
    let app = common::app(state);

    let body = json!({
        "model": "model-a",
        "store": false,
        "tools": [{"type": "function", "name": "shell", "parameters": {"type": "object"}}],
        "input": [
            {"role": "user", "content": "read a.rs then b.rs"},
            {"type": "function_call", "call_id": "call_1", "name": "shell", "arguments": "{\"cmd\":\"cat a.rs\"}"},
            {"type": "function_call_output", "call_id": "call_1", "output": "fn a() {}"},
        ],
    });
    let out = json_body(app.serve(create_req(&bearer, &body)).await.unwrap()).await;
    assert_eq!(out["output"][0]["type"], "function_call");
    assert_eq!(out["output"][0]["call_id"], "call_2");
    assert_eq!(out["output"][0]["name"], "shell");
    assert_eq!(out["output"][0]["arguments"], "{\"cmd\":\"cat b.rs\"}");

    let sent = &upstream_bodies(&upstream).await[0];
    assert_eq!(sent["tools"][0]["function"]["name"], "shell");
    assert_eq!(sent["messages"][1]["tool_calls"][0]["id"], "call_1");
    assert_eq!(
        sent["messages"][2],
        json!({"role": "tool", "tool_call_id": "call_1", "content": "fn a() {}"})
    );
}

// ------------------------------------------------------- stored responses

#[tokio::test]
async fn a_stored_response_can_be_read_listed_continued_and_deleted() {
    let upstream = MockServer::start().await;
    mount_chat(&upstream, completion("Paris")).await;
    let state = common::state_with_chat_pool(&upstream.uri()).await;
    let bearer = common::seed_user_with_token(&state, "alice").await;
    let app = common::app(state);

    let first = json_body(
        app.serve(create_req(
            &bearer,
            &json!({"model": "model-a", "instructions": "be terse", "input": "Capital of France?"}),
        ))
        .await
        .unwrap(),
    )
    .await;
    let id = first["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("resp_"));
    assert_eq!(first["store"], true);

    let read = app
        .serve(bearer_req(
            Method::GET,
            &bearer,
            &format!("/v1/responses/{id}"),
        ))
        .await
        .unwrap();
    assert_eq!(read.status(), StatusCode::OK);
    assert_eq!(json_body(read).await, first);

    let listed = json_body(
        app.serve(bearer_req(
            Method::GET,
            &bearer,
            &format!("/v1/responses/{id}/input_items"),
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(listed["object"], "list");
    assert_eq!(
        listed["data"][0]["content"][0]["text"],
        "Capital of France?"
    );

    let second = app
        .serve(create_req(
            &bearer,
            &json!({"model": "model-a", "previous_response_id": id, "input": "And of Spain?"}),
        ))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(json_body(second).await["previous_response_id"], id.as_str());
    let continued = &upstream_bodies(&upstream).await[1];
    assert_eq!(
        continued["messages"],
        json!([
            {"role": "user", "content": "Capital of France?"},
            {"role": "assistant", "content": "Paris"},
            {"role": "user", "content": "And of Spain?"},
        ]),
        "the stored turn leads, and its instructions are not carried over"
    );

    let deleted = app
        .serve(bearer_req(
            Method::DELETE,
            &bearer,
            &format!("/v1/responses/{id}"),
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    assert_eq!(
        json_body(deleted).await,
        json!({"id": id, "object": "response.deleted", "deleted": true})
    );
    let gone = app
        .serve(bearer_req(
            Method::GET,
            &bearer,
            &format!("/v1/responses/{id}"),
        ))
        .await
        .unwrap();
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn another_persons_stored_response_is_invisible() {
    let upstream = MockServer::start().await;
    mount_chat(&upstream, completion("secret")).await;
    let state = common::state_with_chat_pool(&upstream.uri()).await;
    let alice = common::seed_user_with_token(&state, "alice").await;
    let bob = common::seed_user_with_token(&state, "bob").await;
    let app = common::app(state);

    let created = json_body(
        app.serve(create_req(
            &alice,
            &json!({"model": "model-a", "input": "hi"}),
        ))
        .await
        .unwrap(),
    )
    .await;
    let id = created["id"].as_str().unwrap();

    for (method, uri) in [
        (Method::GET, format!("/v1/responses/{id}")),
        (Method::GET, format!("/v1/responses/{id}/input_items")),
        (Method::DELETE, format!("/v1/responses/{id}")),
    ] {
        let resp = app
            .serve(bearer_req(method.clone(), &bob, &uri))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{method} {uri}");
    }
    let continued = app
        .serve(create_req(
            &bob,
            &json!({"model": "model-a", "previous_response_id": id, "input": "more"}),
        ))
        .await
        .unwrap();
    assert_eq!(continued.status(), StatusCode::BAD_REQUEST);
    let body = json_body(continued).await;
    assert_eq!(body["error"]["code"], "previous_response_not_found");
    assert_eq!(body["error"]["param"], "previous_response_id");
    assert_eq!(
        upstream_bodies(&upstream).await.len(),
        1,
        "bob's request never ran"
    );

    let mine = app
        .serve(bearer_req(
            Method::GET,
            &alice,
            &format!("/v1/responses/{id}"),
        ))
        .await
        .unwrap();
    assert_eq!(
        mine.status(),
        StatusCode::OK,
        "bob's delete did not touch it"
    );
}

#[tokio::test]
async fn a_response_with_store_off_is_not_kept() {
    let upstream = MockServer::start().await;
    mount_chat(&upstream, completion("ok")).await;
    let state = common::state_with_chat_pool(&upstream.uri()).await;
    let bearer = common::seed_user_with_token(&state, "alice").await;
    let app = common::app(state);

    let created = json_body(
        app.serve(create_req(
            &bearer,
            &json!({"model": "model-a", "input": "hi", "store": false}),
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(created["store"], false);
    let id = created["id"].as_str().unwrap();
    let read = app
        .serve(bearer_req(
            Method::GET,
            &bearer,
            &format!("/v1/responses/{id}"),
        ))
        .await
        .unwrap();
    assert_eq!(read.status(), StatusCode::NOT_FOUND);
}

// ------------------------------------------------------------ streaming

#[tokio::test]
async fn a_streamed_turn_emits_the_responses_event_sequence_and_is_stored_first() {
    let upstream = MockServer::start().await;
    mount_stream(
        &upstream,
        "data: {\"id\":\"c1\",\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"Hel\"}}]}\n\n\
         data: {\"id\":\"c1\",\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n\
         data: {\"id\":\"c1\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
         data: {\"id\":\"c1\",\"choices\":[],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":2}}\n\n\
         data: [DONE]\n\n",
    )
    .await;
    let state = common::state_with_chat_pool(&upstream.uri()).await;
    let bearer = common::seed_user_with_token(&state, "alice").await;
    let app = common::app(state);

    let events = stream_events(
        &app,
        &bearer,
        &json!({"model": "model-a", "input": "hi", "stream": true}),
    )
    .await;
    let names: Vec<&str> = events.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "response.created",
            "response.in_progress",
            "response.output_item.added",
            "response.content_part.added",
            "response.output_text.delta",
            "response.output_text.delta",
            "response.output_text.done",
            "response.content_part.done",
            "response.output_item.done",
            "response.completed",
        ]
    );
    let sequence: Vec<u64> = events
        .iter()
        .map(|(_, d)| {
            d["sequence_number"]
                .as_u64()
                .expect("every event is numbered")
        })
        .collect();
    assert_eq!(sequence, (0..events.len() as u64).collect::<Vec<_>>());
    let completed = &events.last().unwrap().1["response"];
    assert_eq!(completed["output"][0]["content"][0]["text"], "Hello");
    assert_eq!(completed["usage"]["input_tokens"], 7);
    assert_eq!(completed["usage"]["output_tokens"], 2);

    let id = completed["id"].as_str().unwrap();
    let stored = json_body(
        app.serve(bearer_req(
            Method::GET,
            &bearer,
            &format!("/v1/responses/{id}"),
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(stored["output"], completed["output"]);
}

#[tokio::test]
async fn a_streamed_client_tool_call_is_a_function_call_item() {
    let upstream = MockServer::start().await;
    mount_stream(
        &upstream,
        "data: {\"id\":\"c1\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_9\",\"type\":\"function\",\"function\":{\"name\":\"shell\",\"arguments\":\"{\\\"cmd\\\":\"}}]}}]}\n\n\
         data: {\"id\":\"c1\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"ls\\\"}\"}}]}}]}\n\n\
         data: {\"id\":\"c1\",\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n\
         data: [DONE]\n\n",
    )
    .await;
    let state = common::state_with_chat_pool(&upstream.uri()).await;
    let bearer = common::seed_user_with_token(&state, "alice").await;
    let app = common::app(state);

    let events = stream_events(
        &app,
        &bearer,
        &json!({
            "model": "model-a",
            "stream": true,
            "store": false,
            "tools": [{"type": "function", "name": "shell", "parameters": {"type": "object"}}],
            "input": "list files",
        }),
    )
    .await;
    let done = events
        .iter()
        .find(|(n, _)| n == "response.output_item.done")
        .expect("the call is an output item");
    assert_eq!(done.1["item"]["type"], "function_call");
    assert_eq!(done.1["item"]["call_id"], "call_9");
    assert_eq!(done.1["item"]["arguments"], "{\"cmd\":\"ls\"}");
    assert_eq!(events.last().unwrap().0, "response.completed");
}

#[tokio::test]
async fn a_mid_stream_upstream_failure_is_response_failed() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": {"message": "too many concurrent requests"}
        })))
        .mount(&upstream)
        .await;
    let state = common::state_with_chat_pool(&upstream.uri()).await;
    let bearer = common::seed_user_with_token(&state, "alice").await;
    let app = common::app(state);

    let events = stream_events(
        &app,
        &bearer,
        &json!({"model": "model-a", "input": "hi", "stream": true}),
    )
    .await;
    let (name, payload) = events.last().unwrap();
    assert_eq!(name, "response.failed");
    let error = &payload["response"]["error"];
    assert_eq!(error["code"], "rate_limit_exceeded");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("too many concurrent requests"),
        "the upstream's wording survives: {error}"
    );
}

// -------------------------------------------------------------- errors

#[tokio::test]
async fn an_upstream_error_is_relayed_with_its_status_and_wording() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {"message": "maximum context length is 8192 tokens", "type": "BadRequestError"}
        })))
        .mount(&upstream)
        .await;
    let state = common::state_with_chat_pool(&upstream.uri()).await;
    let bearer = common::seed_user_with_token(&state, "alice").await;
    let app = common::app(state);

    let resp = app
        .serve(create_req(
            &bearer,
            &json!({"model": "model-a", "input": "hi"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(resp).await["error"]["message"],
        "maximum context length is 8192 tokens"
    );
}

#[tokio::test]
async fn requests_it_cannot_honour_are_400_naming_the_parameter() {
    let state = common::state_with_chat_pool("http://unused.invalid").await;
    let bearer = common::seed_user_with_token(&state, "alice").await;
    let app = common::app(state);

    for (body, param) in [
        (json!({"input": "hi"}), "model"),
        (
            json!({"model": "model-a", "input": "hi", "background": true}),
            "background",
        ),
        (json!({"model": "model-a", "input": 3}), "input"),
    ] {
        let resp = app.serve(create_req(&bearer, &body)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{body}");
        let error = json_body(resp).await;
        assert_eq!(error["error"]["type"], "invalid_request_error");
        assert_eq!(error["error"]["param"], param, "{body}");
    }
}

#[tokio::test]
async fn an_unknown_model_is_404_model_not_found() {
    let state = common::state_with_chat_pool("http://unused.invalid").await;
    let bearer = common::seed_user_with_token(&state, "alice").await;
    let app = common::app(state);

    let resp = app
        .serve(create_req(
            &bearer,
            &json!({"model": "no-such-model", "input": "hi"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(json_body(resp).await["error"]["code"], "model_not_found");
}

#[tokio::test]
async fn a_quota_breach_is_429_with_retry_after() {
    use aiplane_core::server::db::limits::{self, Dimension, SubjectType, Window};

    let state = common::state_with_chat_pool("http://unused.invalid").await;
    let bearer = common::seed_user_with_token(&state, "alice").await;
    limits::upsert(
        &state.db,
        SubjectType::Global,
        "",
        None,
        Dimension::Requests,
        Window::Hour,
        0.0,
    )
    .await
    .unwrap();
    let app = common::app(state);

    let resp = app
        .serve(create_req(
            &bearer,
            &json!({"model": "model-a", "input": "hi"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(resp.headers().get("retry-after").is_some());
    assert_eq!(
        json_body(resp).await["error"]["code"],
        "rate_limit_exceeded"
    );
}

/// Limits gate the resolved model's pool: an exempt pool answers after the
/// budget is spent, whoever's rule spent it, and an enforced pool refuses on
/// the owner's and on the token's own rules alike.
#[tokio::test]
async fn limits_follow_the_pools_enforcement() {
    use aiplane_core::server::db::limits::{self, Dimension, SubjectType, Window};
    use aiplane_core::server::upstreams::PoolKind;

    for (enforce_limits, subject, expected) in [
        (false, SubjectType::Global, StatusCode::OK),
        (false, SubjectType::Token, StatusCode::OK),
        (true, SubjectType::Global, StatusCode::TOO_MANY_REQUESTS),
        (true, SubjectType::Token, StatusCode::TOO_MANY_REQUESTS),
    ] {
        let upstream = MockServer::start().await;
        mount_chat(&upstream, completion("hi")).await;
        let state = common::state_with_pool_enforcing(
            &upstream.uri(),
            PoolKind::Chat,
            "model-a",
            enforce_limits,
        )
        .await;
        let (bearer, token_id) = common::seed_user_with_token_id(&state, "alice").await;
        let subject_id = match subject {
            SubjectType::Token => token_id.clone(),
            _ => String::new(),
        };
        limits::upsert(
            &state.db,
            subject,
            &subject_id,
            None,
            Dimension::Requests,
            Window::Hour,
            0.0,
        )
        .await
        .unwrap();

        let resp = common::app(state)
            .serve(create_req(
                &bearer,
                &json!({"model": "model-a", "input": "hi"}),
            ))
            .await
            .unwrap();

        let case = format!("enforce_limits={enforce_limits}, rule on {subject:?}");
        assert_eq!(resp.status(), expected, "{case}");
        let reached = upstream.received_requests().await.unwrap().len();
        assert_eq!(reached, usize::from(expected == StatusCode::OK), "{case}");
    }
}

#[tokio::test]
async fn an_alias_is_reported_as_asked_and_resolved_in_the_header() {
    let upstream = MockServer::start().await;
    mount_chat(&upstream, completion("aliased")).await;
    let state = common::state_with_alias(&upstream.uri(), "gpt-5-codex", "model-a").await;
    let bearer = common::seed_user_with_token(&state, "alice").await;
    let app = common::app(state);

    let resp = app
        .serve(create_req(
            &bearer,
            &json!({"model": "gpt-5-codex", "input": "hi"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resolved = resp
        .headers()
        .get("x-gateway-resolved-model")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    assert_eq!(json_body(resp).await["model"], "gpt-5-codex");
    assert_eq!(resolved.as_deref(), Some("model-a"));
    assert_eq!(upstream_bodies(&upstream).await[0]["model"], "model-a");
}
