// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

use crate::common;

use aiplane_core::server::db::automatic_routes::{self, AutomaticRoute, AutomaticRouteCandidate};
use aiplane_core::server::db::limits::ManagedBy;
use aiplane_core::server::db::token_models;
use common::Service as _;
use rama::http::{Body, Method, Request, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn route(rollout: &str, minimum_confidence: f64) -> AutomaticRoute {
    AutomaticRoute {
        alias: "default".into(),
        selector_model: "jev-model".into(),
        objective: "balanced".into(),
        instructions: "Use expert for complex software engineering.".into(),
        minimum_confidence,
        selector_timeout_ms: 1_000,
        fallback_target: "fast-model".into(),
        session_affinity: false,
        session_ttl_seconds: 3_600,
        rollout: rollout.into(),
        version: 0,
        candidates: vec![
            AutomaticRouteCandidate {
                key: "fast".into(),
                target: "fast-model".into(),
                description: "Fast inexpensive general model".into(),
            },
            AutomaticRouteCandidate {
                key: "expert".into(),
                target: "expert-model".into(),
                description: "Strong software engineering model".into(),
            },
        ],
    }
}

async fn request(state: aiplane::rama_server::RamaState) -> rama::http::Response {
    let bearer = common::seed_user_with_token(&state, "alice").await;
    common::app(state)
        .serve(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/chat/completions")
                .header("authorization", format!("Bearer {bearer}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "default",
                        "messages": [{"role": "user", "content": "Fix this Rust lifetime"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn mount_selector(upstream: &MockServer, confidence: f64) {
    Mock::given(method("POST"))
        .and(path("/systemone"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "jev-model",
            "answers": {
                "route": {
                    "type": "choice",
                    "choice": "expert",
                    "confidence": confidence,
                    "probabilities": {"fast": 1.0 - confidence, "expert": confidence}
                }
            },
            "usage": {"input_tokens": 20, "output_tokens": 2}
        })))
        .mount(upstream)
        .await;
}

async fn wait_for_latest_decision_reason(db: &aiplane_core::server::db::Pool) -> String {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(reason) = sqlx::query_scalar(
                "SELECT reason FROM automatic_route_decisions ORDER BY id DESC LIMIT 1",
            )
            .fetch_optional(db)
            .await
            .unwrap()
            {
                return reason;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("automatic routing decision was not persisted within 2s")
}

#[tokio::test]
async fn active_route_selects_a_candidate_and_exposes_the_decision() {
    let upstream = MockServer::start().await;
    mount_selector(&upstream, 0.92).await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_json(json!({
            "model": "expert-model",
            "messages": [{"role": "user", "content": "Fix this Rust lifetime"}]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "expert-model",
            "choices": [{"message": {"role": "assistant", "content": "done"}}]
        })))
        .mount(&upstream)
        .await;
    let state = common::state_with_automatic_route_pools(&upstream.uri()).await;
    automatic_routes::upsert(&state.db, &route("active", 0.7))
        .await
        .unwrap();

    let response = request(state).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-gateway-route-target")
            .and_then(|value| value.to_str().ok()),
        Some("expert-model")
    );
    assert_eq!(
        response
            .headers()
            .get("x-gateway-route-reason")
            .and_then(|value| value.to_str().ok()),
        Some("selected")
    );
    assert_eq!(
        response
            .headers()
            .get("x-gateway-resolved-model")
            .and_then(|value| value.to_str().ok()),
        Some("expert-model")
    );
}

#[tokio::test]
async fn shadow_route_records_the_suggestion_but_sends_the_fallback() {
    let upstream = MockServer::start().await;
    mount_selector(&upstream, 0.97).await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_json(json!({
            "model": "fast-model",
            "messages": [{"role": "user", "content": "Fix this Rust lifetime"}]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "fast-model",
            "choices": [{"message": {"role": "assistant", "content": "fallback"}}]
        })))
        .mount(&upstream)
        .await;
    let state = common::state_with_automatic_route_pools(&upstream.uri()).await;
    let db = state.db.clone();
    automatic_routes::upsert(&db, &route("shadow", 0.7))
        .await
        .unwrap();

    let response = request(state).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-gateway-route-target")
            .and_then(|value| value.to_str().ok()),
        Some("fast-model")
    );
    assert_eq!(
        response
            .headers()
            .get("x-gateway-route-suggested-target")
            .and_then(|value| value.to_str().ok()),
        Some("expert-model")
    );
    let reason = wait_for_latest_decision_reason(&db).await;
    assert_eq!(reason, "shadow");
}

#[tokio::test]
async fn low_confidence_uses_the_configured_fallback() {
    let upstream = MockServer::start().await;
    mount_selector(&upstream, 0.4).await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "fast-model",
            "choices": [{"message": {"role": "assistant", "content": "fallback"}}]
        })))
        .mount(&upstream)
        .await;
    let state = common::state_with_automatic_route_pools(&upstream.uri()).await;
    automatic_routes::upsert(&state.db, &route("active", 0.8))
        .await
        .unwrap();

    let response = request(state).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-gateway-route-reason")
            .and_then(|value| value.to_str().ok()),
        Some("low_confidence")
    );
    let body: Value = serde_json::from_slice(&common::read_body(response).await).unwrap();
    assert_eq!(body["model"], "fast-model");
}

#[tokio::test]
async fn malformed_selector_response_uses_the_configured_fallback() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/systemone"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"answers": {}})))
        .mount(&upstream)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "fast-model",
            "choices": [{"message": {"role": "assistant", "content": "fallback"}}]
        })))
        .mount(&upstream)
        .await;
    let state = common::state_with_automatic_route_pools(&upstream.uri()).await;
    automatic_routes::upsert(&state.db, &route("active", 0.7))
        .await
        .unwrap();

    let response = request(state).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-gateway-route-reason")
            .and_then(|value| value.to_str().ok()),
        Some("selector_error")
    );
}

#[tokio::test]
async fn selector_timeout_uses_the_configured_fallback() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/systemone"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_millis(250))
                .set_body_json(json!({})),
        )
        .mount(&upstream)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "fast-model",
            "choices": [{"message": {"role": "assistant", "content": "fallback"}}]
        })))
        .mount(&upstream)
        .await;
    let state = common::state_with_automatic_route_pools(&upstream.uri()).await;
    let mut policy = route("active", 0.7);
    policy.selector_timeout_ms = 100;
    automatic_routes::upsert(&state.db, &policy).await.unwrap();

    let response = request(state).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-gateway-route-reason")
            .and_then(|value| value.to_str().ok()),
        Some("selector_error")
    );
}

#[tokio::test]
async fn anthropic_messages_use_the_same_automatic_route() {
    let upstream = MockServer::start().await;
    mount_selector(&upstream, 0.95).await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-auto",
            "model": "expert-model",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": {"role": "assistant", "content": "selected"}
            }],
            "usage": {"prompt_tokens": 4, "completion_tokens": 1, "total_tokens": 5}
        })))
        .mount(&upstream)
        .await;
    let state = common::state_with_automatic_route_pools(&upstream.uri()).await;
    automatic_routes::upsert(&state.db, &route("active", 0.7))
        .await
        .unwrap();
    let bearer = common::seed_user_with_token(&state, "alice").await;
    let response = common::app(state)
        .serve(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/messages")
                .header("authorization", format!("Bearer {bearer}"))
                .header("content-type", "application/json")
                .header("anthropic-version", "2023-06-01")
                .body(Body::from(
                    json!({
                        "model": "default",
                        "max_tokens": 256,
                        "messages": [{"role": "user", "content": "Fix this Rust lifetime"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-gateway-route-target")
            .and_then(|value| value.to_str().ok()),
        Some("expert-model")
    );
    let body: Value = serde_json::from_slice(&common::read_body(response).await).unwrap();
    assert_eq!(body["model"], "default");
    assert_eq!(body["content"][0]["text"], "selected");
}

#[tokio::test]
async fn anthropic_token_count_uses_the_route_and_exposes_the_decision() {
    let upstream = MockServer::start().await;
    mount_selector(&upstream, 0.95).await;
    Mock::given(method("POST"))
        .and(path("/tokenize"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"count": 42})))
        .mount(&upstream)
        .await;
    let state = common::state_with_automatic_route_pools(&upstream.uri()).await;
    automatic_routes::upsert(&state.db, &route("active", 0.7))
        .await
        .unwrap();
    let bearer = common::seed_user_with_token(&state, "alice").await;

    let response = common::app(state)
        .serve(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/messages/count_tokens")
                .header("authorization", format!("Bearer {bearer}"))
                .header("content-type", "application/json")
                .header("anthropic-version", "2023-06-01")
                .body(Body::from(
                    json!({
                        "model": "default",
                        "messages": [{"role": "user", "content": "Fix this Rust lifetime"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-gateway-route-target")
            .and_then(|value| value.to_str().ok()),
        Some("expert-model")
    );
    let body: Value = serde_json::from_slice(&common::read_body(response).await).unwrap();
    assert_eq!(body["input_tokens"], 42);
}

#[tokio::test]
async fn anthropic_token_count_does_not_bypass_quota_for_selector_inference() {
    use aiplane_core::server::db::limits::{self, Dimension, SubjectType, Window};

    let upstream = MockServer::start().await;
    mount_selector(&upstream, 0.95).await;
    let state = common::state_with_automatic_route_pools(&upstream.uri()).await;
    automatic_routes::upsert(&state.db, &route("active", 0.7))
        .await
        .unwrap();
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

    let response = common::app(state)
        .serve(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/messages/count_tokens")
                .header("authorization", format!("Bearer {bearer}"))
                .header("content-type", "application/json")
                .header("anthropic-version", "2023-06-01")
                .body(Body::from(
                    json!({
                        "model": "default",
                        "messages": [{"role": "user", "content": "hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let requests = upstream.received_requests().await.unwrap();
    assert!(
        requests.is_empty(),
        "quota rejection must happen before selector inference"
    );
}

#[tokio::test]
async fn a_token_can_grant_the_virtual_alias_without_granting_its_internals() {
    let upstream = MockServer::start().await;
    mount_selector(&upstream, 0.95).await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "expert-model",
            "choices": [{"message": {"role": "assistant", "content": "selected"}}]
        })))
        .mount(&upstream)
        .await;
    let state = common::state_with_automatic_route_pools(&upstream.uri()).await;
    automatic_routes::upsert(&state.db, &route("active", 0.7))
        .await
        .unwrap();
    let (bearer, token_id) = common::seed_user_with_token_id(&state, "alice").await;
    token_models::set_for_token(&state.db, &token_id, &["default".into()], ManagedBy::Owner)
        .await
        .unwrap();
    let app = common::app(state);

    let listed = app
        .serve(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/models")
                .header("authorization", format!("Bearer {bearer}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let listed: Value = serde_json::from_slice(&common::read_body(listed).await).unwrap();
    let ids: Vec<&str> = listed["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|model| model["id"].as_str())
        .collect();
    assert_eq!(ids, vec!["default"]);

    let retrieved = app
        .serve(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/models/default")
                .header("authorization", format!("Bearer {bearer}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(retrieved.status(), StatusCode::OK);

    let response = app
        .serve(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/chat/completions")
                .header("authorization", format!("Bearer {bearer}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"model": "default", "messages": [{"role": "user", "content": "code"}]})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// An agent whose spec names `model`, granted `granted`, on the automatic
/// route fixture's pools; its live version answers through the real driver.
async fn routed_agent(
    state: &aiplane::rama_server::RamaState,
    model: &str,
    granted: &[&str],
) -> String {
    use aiplane_agents::db::{agents, system_principals};
    use aiplane_core::server::principal::GrantKind;
    common::seed_session(state, "owner", "owner@example.com").await;
    let spec = json!({ "main": {
        "model": model,
        "instructions": { "orchestration": "Answer." }
    } })
    .to_string();
    let row = agents::create(
        &state.db,
        &system_principals::NewPrincipal {
            name: "routed",
            display: "Routed",
            description: "",
        },
        &spec,
        "owner",
    )
    .await
    .unwrap()
    .unwrap();
    let id = row.principal.id;
    for model in granted {
        system_principals::add_grant(&state.db, &id, GrantKind::Model, model, "owner")
            .await
            .unwrap();
    }
    agents::publish(&state.db, &id, &spec, "owner")
        .await
        .unwrap()
        .unwrap();
    id
}

async fn mount_streaming_answer(upstream: &MockServer, answer: &str) {
    let sse = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices": [{"index": 0, "delta": {"content": answer}}]})
    );
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .mount(upstream)
        .await;
}

fn agent_turn(agent_id: &str) -> aiplane_runtime::agents::run::AgentTurn<'_> {
    aiplane_runtime::agents::run::AgentTurn {
        agent_id,
        session_id: None,
        message: "Fix this Rust lifetime",
        visitor_id: None,
        lang: None,
    }
}

/// An agent granted an automatic route runs on it like a chat does: the
/// selector picks the candidate, and the turn goes to that model, which the
/// grant on the route covers.
#[tokio::test]
async fn an_agent_granted_an_automatic_route_answers_through_the_selected_candidate() {
    let upstream = MockServer::start().await;
    mount_selector(&upstream, 0.92).await;
    mount_streaming_answer(&upstream, "Routed answer.").await;
    let state = common::state_with_automatic_route_pools(&upstream.uri()).await;
    automatic_routes::upsert(&state.db, &route("active", 0.7))
        .await
        .unwrap();
    let agent = routed_agent(&state, "default", &["default"]).await;

    let reply =
        aiplane_runtime::agents::run::run_turn(&std::sync::Arc::new(state), agent_turn(&agent))
            .await
            .unwrap();

    assert_eq!(reply.answer.as_deref(), Some("Routed answer."), "{reply:?}");
    let chat: Vec<Value> = upstream
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/chat/completions")
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    assert_eq!(chat.len(), 1, "{chat:?}");
    assert_eq!(chat[0]["model"], "expert-model", "the selector's choice");
}

/// A grant on one of the route's candidates is no grant on the route.
#[tokio::test]
async fn an_agent_naming_a_route_it_holds_no_grant_on_does_not_run() {
    let upstream = MockServer::start().await;
    mount_selector(&upstream, 0.92).await;
    mount_streaming_answer(&upstream, "leaked").await;
    let state = common::state_with_automatic_route_pools(&upstream.uri()).await;
    automatic_routes::upsert(&state.db, &route("active", 0.7))
        .await
        .unwrap();
    let agent = routed_agent(&state, "default", &["fast-model"]).await;

    let err =
        aiplane_runtime::agents::run::run_turn(&std::sync::Arc::new(state), agent_turn(&agent))
            .await
            .unwrap_err();

    assert!(
        matches!(
            err,
            aiplane_runtime::agents::profile::AgentRunError::ModelNotGranted { ref model, .. }
                if model == "default"
        ),
        "{err}"
    );
    assert!(upstream.received_requests().await.unwrap().is_empty());
}

/// The agent builder offers the models the manager may grant: an automatic
/// route only when they may use every model it reaches. The chat picker
/// keeps listing a route whose fallback they reach, as it always did.
#[tokio::test]
async fn agent_resources_offer_only_the_automatic_routes_a_manager_may_use_whole() {
    use aiplane_core::server::db::{gateway_groups, users};
    use aiplane_core::server::feature_defaults::{self, Feature};
    use aiplane_core::server::upstreams::{self, PickerStrategy, PoolKind, UpstreamPoolConfig};
    use std::collections::HashMap;

    let upstream = MockServer::start().await;
    let pool = |kind: PoolKind, groups: &[&str]| UpstreamPoolConfig {
        voices: Default::default(),
        offer_voices: Vec::new(),
        allowed_groups: groups.iter().map(|g| g.to_string()).collect(),
        fallback_offline: None,
        compliance: Default::default(),
        enforce_limits: true,
        kind,
        strategy: PickerStrategy::RoundRobin,
        models: Vec::new(),
        backend: vec![common::mock_backend("b", &upstream.uri())],
    };
    let pools = HashMap::from([
        ("chat".to_string(), pool(PoolKind::Chat, &[])),
        ("vip".to_string(), pool(PoolKind::Chat, &["vip"])),
        ("selector".to_string(), pool(PoolKind::SystemOne, &[])),
    ]);
    let registry = upstreams::UpstreamRegistry::new(&pools).unwrap();
    common::seed_pool_models(&registry, "chat", 0, &["fast-model", "expert-model"]);
    common::seed_pool_models(&registry, "vip", 0, &["vip-model"]);
    common::seed_pool_models(&registry, "selector", 0, &["jev-model"]);
    let db = aiplane_core::server::db::open(std::path::Path::new(":memory:"))
        .await
        .unwrap();
    let state = common::state_from_registry(db, registry);
    automatic_routes::upsert(&state.db, &route("active", 0.7))
        .await
        .unwrap();
    let mut vip_route = route("active", 0.7);
    vip_route.alias = "vip-auto".into();
    vip_route.candidates[1].target = "vip-model".into();
    automatic_routes::upsert(&state.db, &vip_route)
        .await
        .unwrap();
    feature_defaults::set(&state.db, Feature::Chat, Some("expert-model"))
        .await
        .unwrap();
    gateway_groups::upsert_group(&state.db, "managers", "", false, false)
        .await
        .unwrap();
    gateway_groups::set_can_manage_agents(&state.db, "managers", true)
        .await
        .unwrap();
    gateway_groups::set_mappings_for_group(&state.db, "managers", &["managers".into()])
        .await
        .unwrap();
    state.reload_rbac().await;
    let now = jiff::Timestamp::now();
    users::upsert(
        &state.db,
        &users::User {
            id: "alice".into(),
            email: "alice@example.com".into(),
            name: None,
            roles: vec!["managers".into()],
            created_at: now,
            updated_at: now,
            timezone: None,
            speech_voice: None,
        },
    )
    .await
    .unwrap();
    let session = state.sessions.create("alice").await.unwrap();
    let cookie = state.sessions.sign(&session.id);
    let app = common::app(state);
    let get = |uri: &str| {
        Request::builder()
            .method(Method::GET)
            .uri(uri)
            .header("cookie", format!("id={cookie}"))
            .body(Body::empty())
            .unwrap()
    };
    let ids = |body: &Value, list: &str| -> Vec<String> {
        body.pointer(list)
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("no {list}: {body}"))
            .iter()
            .map(|m| m["id"].as_str().unwrap().to_string())
            .collect()
    };

    let resp = app.serve(get("/api/v0/agent-resources")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(&common::read_body(resp).await).unwrap();
    assert_eq!(
        ids(&body, "/models/chat"),
        ["expert-model", "default", "fast-model"],
        "the default first; `vip-auto` would hand on `vip-model`"
    );
    assert_eq!(body["defaults"]["chat"], "expert-model");

    let resp = app.serve(get("/api/v0/models")).await.unwrap();
    let body: Value = serde_json::from_slice(&common::read_body(resp).await).unwrap();
    assert!(
        ids(&body, "/models").contains(&"vip-auto".to_string()),
        "the chat picker lists a route whose fallback the person reaches: {body}"
    );
}
