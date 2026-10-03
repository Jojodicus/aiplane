// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The agent builder's internal test chat end to end
//! (`docs/agents.md` "What #90 built"): `POST /api/v0/agents/{id}/test-turn` runs the
//! **draft** through the real run path on wiremock upstreams and returns the
//! answer with the debug view only a manager gets.
//!
//! `alice` and `bob` hold `can_manage_agents`; `plain` holds nothing. The
//! main agent (`support`) sets an `issue` slot, then `forward_request`
//! reaches the `tech` sub-agent, which finishes.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rama::Service;
use rama::http::{Body, Method, Request, StatusCode};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::common::{self, TEST_SECRET};

use aiplane::rama_server::{RamaState, SessionStore};
use aiplane_core::server::db::{self, gateway_groups, users};
use aiplane_core::server::rbac::Resolver;
use aiplane_core::server::upstreams::{
    self,
    config::{PickerStrategy, PoolKind, UpstreamPoolConfig},
};
use aiplane_runtime::server::AppState;
use aiplane_runtime::server::tools::ToolRegistry;
use aiplane_runtime::server::tools::time::CurrentTimestamp;

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

async fn llm(deltas: Vec<Value>) -> MockServer {
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
    json!({ "tool_calls": [{"index": 0, "id": id, "type": "function",
        "function": {"name": name, "arguments": args.to_string()}}] })
}

fn text(s: &str) -> Value {
    json!({ "content": s })
}

async fn requests(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect()
}

struct Fx {
    state: RamaState,
    alice: String,
    bob: String,
    plain: String,
    main_llm: MockServer,
    /// Kept alive for the fixture's `guard-pool`.
    _guard_llm: MockServer,
}

/// A topic guard upstream that judges every message out of scope.
async fn off_topic_guard() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "role": "assistant",
                                       "content": json!({ "verdict": "out_of_scope" }).to_string() } }]
        })))
        .mount(&server)
        .await;
    server
}

async fn person(state: &RamaState, id: &str, roles: &[&str]) -> String {
    let now = jiff::Timestamp::now();
    users::upsert(
        &state.db,
        &users::User {
            id: id.into(),
            email: format!("{id}@example.com"),
            name: None,
            roles: roles.iter().map(|r| r.to_string()).collect(),
            created_at: now,
            updated_at: now,
            timezone: None,
            speech_voice: None,
        },
    )
    .await
    .unwrap();
    let session = state.sessions.create(id).await.unwrap();
    state.sessions.sign(&session.id)
}

fn chat_pool(upstream: &MockServer) -> UpstreamPoolConfig {
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
        backend: vec![common::mock_backend("mock", &upstream.uri())],
    }
}

async fn fixture(main_script: Vec<Value>, tech_script: Vec<Value>) -> (Fx, MockServer) {
    let pool = db::open(std::path::Path::new(":memory:")).await.unwrap();
    let main_llm = llm(main_script).await;
    let tech_llm = llm(tech_script).await;
    let guard_llm = off_topic_guard().await;
    let mut pools = HashMap::new();
    pools.insert("main-pool".to_string(), chat_pool(&main_llm));
    pools.insert("tech-pool".to_string(), chat_pool(&tech_llm));
    pools.insert("guard-pool".to_string(), chat_pool(&guard_llm));
    let registry = upstreams::UpstreamRegistry::new(&pools).unwrap();
    common::seed_pool_models(&registry, "main-pool", 0, &["main-model"]);
    common::seed_pool_models(&registry, "tech-pool", 0, &["tech-model"]);
    common::seed_pool_models(&registry, "guard-pool", 0, &["guard-model"]);
    let app = AppState::new(
        common::test_config(),
        pool.clone(),
        registry,
        Arc::new(ToolRegistry::new().with(CurrentTimestamp)),
        Arc::new(Resolver::empty()),
    );
    let state = RamaState::new(
        app,
        SessionStore::new(pool.clone(), TEST_SECRET),
        aiplane_core::server::usage::UsageHandle::disabled(),
    );
    gateway_groups::upsert_group(&pool, "everyone", "", false, true)
        .await
        .unwrap();
    gateway_groups::upsert_group(&pool, "managers", "", false, false)
        .await
        .unwrap();
    gateway_groups::set_can_manage_agents(&pool, "managers", true)
        .await
        .unwrap();
    gateway_groups::set_mappings_for_group(&pool, "managers", &["managers".into()])
        .await
        .unwrap();
    gateway_groups::set_tools_for_group(&pool, "everyone", &["get_current_timestamp".into()])
        .await
        .unwrap();
    state.reload_rbac().await;
    let alice = person(&state, "alice", &["managers"]).await;
    let bob = person(&state, "bob", &["managers"]).await;
    let plain = person(&state, "plain", &[]).await;
    (
        Fx {
            state,
            alice,
            bob,
            plain,
            main_llm,
            _guard_llm: guard_llm,
        },
        tech_llm,
    )
}

impl Fx {
    async fn send(
        &self,
        cookie: &str,
        method: Method,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("cookie", format!("id={cookie}"));
        let req = match body {
            Some(b) => req
                .header("content-type", "application/json")
                .body(Body::from(b.to_string())),
            None => req.body(Body::empty()),
        }
        .unwrap();
        let resp = common::app(self.state.clone()).serve(req).await.unwrap();
        let status = resp.status();
        let bytes = common::read_body(resp).await;
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn post(&self, cookie: &str, uri: &str, body: Value) -> (StatusCode, Value) {
        self.send(cookie, Method::POST, uri, Some(body)).await
    }

    async fn create(&self, name: &str, spec: Option<Value>) -> String {
        let mut body = json!({ "name": name });
        if let Some(spec) = spec {
            body["spec"] = spec;
        }
        let (status, body) = self.post(&self.alice, "/api/v0/agents", body).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["agent"]["id"].as_str().unwrap().to_string()
    }

    async fn grant_pool(&self, id: &str, pool: &str) {
        let (status, body) = self
            .post(
                &self.alice,
                &format!("/api/v0/system-principals/{id}/grants"),
                json!({ "kind": "pool", "ref": pool }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    async fn save_draft(&self, id: &str, spec: Value) {
        let (status, body) = self
            .send(
                &self.alice,
                Method::PUT,
                &format!("/api/v0/agents/{id}/draft"),
                Some(json!({ "spec": spec })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    async fn publish(&self, id: &str) {
        let (status, body) = self
            .post(
                &self.alice,
                &format!("/api/v0/agents/{id}/publish"),
                json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    async fn turn(&self, cookie: &str, id: &str, body: Value) -> (StatusCode, Value) {
        self.post(cookie, &format!("/api/v0/agents/{id}/test-turn"), body)
            .await
    }

    /// The published `tech` sub-agent and the unpublished `support` main
    /// agent whose draft routes to it.
    async fn support(&self, orchestration: &str) -> (String, String) {
        let tech = self.create("tech", None).await;
        self.grant_pool(&tech, "tech-pool").await;
        self.save_draft(
            &tech,
            json!({ "main": { "pool": "tech-pool",
                "instructions": { "orchestration": "Solve the technical issue." } },
                "finish": { "schema": { "type": "object", "required": ["answer"],
                    "properties": { "answer": { "type": "string" } } } } }),
        )
        .await;
        self.publish(&tech).await;

        let support = self.create("support", None).await;
        self.grant_pool(&support, "main-pool").await;
        self.save_draft(&support, support_spec(&tech, orchestration))
            .await;
        (support, tech)
    }
}

fn support_spec(tech: &str, orchestration: &str) -> Value {
    json!({
        "main": {
            "pool": "main-pool",
            "instructions": { "orchestration": orchestration },
            "budget": { "rounds": 6 }
        },
        "state": {
            "issue": { "type": "enum", "values": ["billing", "technical"], "set_by": ["llm"] },
            "email": { "type": "email", "set_by": ["llm"] }
        },
        "routes": {
            "technical": {
                "description": "Technical problems",
                "when": { "slot": "issue", "eq": "technical" },
                "agent": tech,
                "task": "Help with the technical issue."
            }
        }
    })
}

fn support_script() -> Vec<Value> {
    vec![
        call("c1", "set_issue", json!({ "value": "technical" })),
        call("c2", "forward_request", json!({})),
        text("The technician looked into it."),
    ]
}

fn tech_script() -> Vec<Value> {
    vec![call(
        "f1",
        "finish",
        json!({ "result": { "answer": "Restart the router." } }),
    )]
}

#[tokio::test]
async fn a_draft_that_was_never_published_answers_with_the_manager_debug_view() {
    let (fx, _tech_llm) = fixture(support_script(), tech_script()).await;
    let (support, tech) = fx.support("Find out what is wrong, then forward it.").await;

    let (status, body) = fx
        .turn(
            &fx.alice,
            &support,
            json!({ "message": "My router is broken" }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "completed", "{body}");
    assert_eq!(body["answer"], "The technician looked into it.");
    assert_eq!(body["draft_version"], 0);
    assert!(body["session_id"].as_str().is_some());

    let slots = body["debug"]["slots"].as_array().unwrap();
    let issue = slots.iter().find(|s| s["slot"] == "issue").unwrap();
    assert_eq!(issue["status"], "set");
    assert_eq!(issue["value"], "technical");
    assert_eq!(issue["provenance"], "llm");
    let email = slots.iter().find(|s| s["slot"] == "email").unwrap();
    assert_eq!(email["status"], "missing");

    let routes = body["debug"]["routes"].as_array().unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0]["route"], "technical");
    assert_eq!(routes[0]["open"], true);

    let routing = body["debug"]["routing"].as_array().unwrap();
    assert_eq!(routing.len(), 1);
    assert_eq!(routing[0]["picked"], "technical");

    let subs = body["debug"]["sub_agents"].as_array().unwrap();
    assert_eq!(subs.len(), 1, "{body}");
    assert_eq!(subs[0]["sub_agent_id"], tech);
    assert_eq!(subs[0]["outcome"]["status"], "finished");
    assert_eq!(
        subs[0]["outcome"]["result"]["answer"],
        "Restart the router."
    );

    let tools: Vec<&str> = body["debug"]["tool_calls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["tool"].as_str().unwrap())
        .collect();
    assert_eq!(tools, ["set_issue", "forward_request"]);
}

#[tokio::test]
async fn the_debug_view_shows_the_topic_guards_verdict() {
    let (fx, _tech_llm) = fixture(vec![text("Diesel engines …")], tech_script()).await;
    let website = fx.create("website", None).await;
    fx.grant_pool(&website, "main-pool").await;
    fx.grant_pool(&website, "guard-pool").await;
    let mut spec = json!({
        "main": { "pool": "main-pool",
                  "instructions": { "orchestration": "Answer questions about Ceph." } },
        "scope": { "topics": ["Ceph storage"], "strict": true, "classifier_pool": "guard-pool" }
    });
    let (status, body) = fx
        .send(
            &fx.alice,
            Method::PUT,
            &format!("/api/v0/agents/{website}/draft"),
            Some(json!({ "spec": spec })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["issues"][0]["path"], "scope.refusal");
    spec["scope"]["refusal"] = json!("I only talk about Ceph.");
    fx.save_draft(&website, spec).await;

    let (status, body) = fx
        .turn(
            &fx.alice,
            &website,
            json!({ "message": "How does a diesel engine work?" }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["answer"], "I only talk about Ceph.");
    assert_eq!(
        body["debug"]["scope"],
        json!({ "verdict": "out_of_scope", "topics": ["Ceph storage"] })
    );
    assert!(requests(&fx.main_llm).await.is_empty());
}

#[tokio::test]
async fn the_test_chat_runs_the_draft_not_the_published_version() {
    let (fx, _tech_llm) = fixture(vec![text("ok")], tech_script()).await;
    let (support, tech) = fx.support("LIVE-INSTRUCTIONS").await;
    fx.publish(&support).await;
    fx.save_draft(&support, support_spec(&tech, "DRAFT-INSTRUCTIONS"))
        .await;

    let (status, body) = fx
        .turn(&fx.alice, &support, json!({ "message": "hello" }))
        .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let seen = requests(&fx.main_llm).await;
    let system = seen[0]["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("DRAFT-INSTRUCTIONS"), "{system}");
    assert!(!system.contains("LIVE-INSTRUCTIONS"), "{system}");
}

#[tokio::test]
async fn a_second_turn_continues_the_conversation_and_keeps_its_state() {
    let (fx, _tech_llm) = fixture(
        vec![
            call("c1", "set_email", json!({ "value": "ada@example.com" })),
            text("Thanks, Ada."),
            text("Anything else?"),
        ],
        tech_script(),
    )
    .await;
    let (support, _) = fx.support("Collect the email.").await;

    let (_, first) = fx
        .turn(
            &fx.alice,
            &support,
            json!({ "message": "I am ada@example.com" }),
        )
        .await;
    let session = first["session_id"].as_str().unwrap();
    let (status, second) = fx
        .turn(
            &fx.alice,
            &support,
            json!({ "message": "that is all", "session_id": session }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["session_id"], session);
    assert_ne!(second["turn_id"], first["turn_id"]);
    let email = second["debug"]["slots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["slot"] == "email")
        .unwrap()
        .clone();
    assert_eq!(email["value"], "ada@example.com");
    assert!(
        second["debug"]["tool_calls"].as_array().unwrap().is_empty(),
        "the debug view lists this turn's calls only: {second}"
    );
}

#[tokio::test]
async fn an_unknown_session_empty_message_and_ungranted_pool_say_what_to_fix() {
    let (fx, _tech_llm) = fixture(vec![text("ok")], tech_script()).await;
    let (support, _) = fx.support("x").await;

    let (status, body) = fx
        .turn(
            &fx.alice,
            &support,
            json!({ "message": "hi", "session_id": "nope" }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "unknown_session");

    let (status, _) = fx
        .turn(&fx.alice, &support, json!({ "message": "  " }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let bare = fx.create("bare", None).await;
    fx.grant_pool(&bare, "tech-pool").await;
    fx.save_draft(&bare, json!({ "main": { "pool": "tech-pool" } }))
        .await;
    let (status, _) = fx.turn(&fx.alice, &bare, json!({ "message": "hi" })).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = fx
        .post(
            &fx.alice,
            &format!("/api/v0/system-principals/{bare}/grants/revoke"),
            json!({ "kind": "pool", "ref": "tech-pool" }),
        )
        .await;
    assert!(status.is_success(), "{status} {body}");
    let (status, body) = fx.turn(&fx.alice, &bare, json!({ "message": "hi" })).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"]["code"], "agent_no_model");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("tech-pool")
    );
}

#[tokio::test]
async fn only_a_manager_with_a_write_share_may_run_the_draft() {
    let (fx, _tech_llm) = fixture(vec![text("ok")], tech_script()).await;
    let (support, _) = fx.support("x").await;
    let message = json!({ "message": "hi" });

    let (status, _) = fx.turn(&fx.plain, &support, message.clone()).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "no agent-management permission"
    );

    let (status, _) = fx.turn(&fx.bob, &support, message.clone()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "no share");

    let (status, _) = fx
        .post(
            &fx.alice,
            &format!("/api/v0/agents/{support}/shares"),
            json!({ "subject_kind": "user", "subject_id": "bob", "access": "read" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = fx.turn(&fx.bob, &support, message).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "agent_write_required");
}

#[tokio::test]
async fn the_identity_says_whether_the_agents_section_is_open_to_the_user() {
    let (fx, _tech_llm) = fixture(vec![text("ok")], tech_script()).await;

    let (_, alice) = fx.send(&fx.alice, Method::GET, "/api/v0/me", None).await;
    let (_, plain) = fx.send(&fx.plain, Method::GET, "/api/v0/me", None).await;

    assert_eq!(alice["can_manage_agents"], true);
    assert_eq!(plain["can_manage_agents"], false);
}

#[tokio::test]
async fn the_builder_is_offered_exactly_what_the_manager_could_grant() {
    let (fx, _tech_llm) = fixture(vec![text("ok")], tech_script()).await;

    let (status, body) = fx
        .send(&fx.alice, Method::GET, "/api/v0/agent-resources", None)
        .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let pools: Vec<&str> = body["pools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(pools, ["guard-pool", "main-pool", "tech-pool"]);
    let tools: Vec<&str> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["id"].as_str())
        .collect();
    assert_eq!(tools, ["get_current_timestamp"]);
    assert!(body["connectors"].as_array().unwrap().is_empty());
    assert!(body["skills"].as_array().unwrap().is_empty());

    let (status, _) = fx
        .send(&fx.plain, Method::GET, "/api/v0/agent-resources", None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn resources_name_the_pool_behind_each_model_choice_an_admin_mapped() {
    use aiplane_core::server::settings;

    let (fx, _tech_llm) = fixture(vec![text("ok")], tech_script()).await;
    let (status, body) = fx
        .send(&fx.alice, Method::GET, "/api/v0/agent-resources", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["tiers"],
        json!({ "fast": null, "balanced": null, "thorough": null })
    );

    settings::store(
        &fx.state.db,
        &fx.state.crypto,
        &[
            ("agents.pool_fast".into(), "guard-pool".into()),
            ("agents.pool_balanced".into(), "main-pool".into()),
        ],
    )
    .await
    .unwrap();
    fx.state.reload_settings().await;

    let (_, body) = fx
        .send(&fx.alice, Method::GET, "/api/v0/agent-resources", None)
        .await;
    assert_eq!(
        body["tiers"],
        json!({ "fast": "guard-pool", "balanced": "main-pool", "thorough": null })
    );

    settings::store(
        &fx.state.db,
        &fx.state.crypto,
        &[("agents.pool_balanced".into(), String::new())],
    )
    .await
    .unwrap();
    fx.state.reload_settings().await;
    let (_, body) = fx
        .send(&fx.alice, Method::GET, "/api/v0/agent-resources", None)
        .await;
    assert!(body["tiers"]["balanced"].is_string(), "{body}");
    assert_eq!(
        body["tiers"]["balanced"], body["defaults"]["chat"]["pool"],
        "an unset Balanced is the gateway's default chat model: {body}"
    );
}

/// The setup assistant's starter templates (`web/src/lib/agent-templates.json`)
/// with every `@key` string translated, as the SPA sends them.
fn templates_in(lang: session_core::i18n::Lang) -> Vec<(String, Value)> {
    fn resolve(v: &Value, lang: session_core::i18n::Lang) -> Value {
        match v {
            Value::String(s) => match s.strip_prefix('@') {
                Some(key) => Value::String(session_core::i18n::t(lang, key)),
                None => v.clone(),
            },
            Value::Array(items) => items.iter().map(|i| resolve(i, lang)).collect(),
            Value::Object(map) => map
                .iter()
                .map(|(k, v)| (k.clone(), resolve(v, lang)))
                .collect(),
            other => other.clone(),
        }
    }
    let all: Value =
        serde_json::from_str(include_str!("../../../../web/src/lib/agent-templates.json")).unwrap();
    all.as_object()
        .unwrap()
        .iter()
        .filter(|(name, _)| !name.starts_with('_'))
        .map(|(name, spec)| (name.clone(), resolve(spec, lang)))
        .collect()
}

#[tokio::test]
async fn every_setup_template_is_a_valid_draft_in_every_language() {
    let (fx, _tech_llm) = fixture(vec![text("ok")], tech_script()).await;
    for lang in session_core::i18n::Lang::ALL {
        let templates = templates_in(lang);
        assert_eq!(templates.len(), 5, "faq, support, leads, internal, blank");
        for (name, spec) in templates {
            let text = spec.to_string();
            assert!(
                !text.contains("\"@"),
                "{name} ({lang:?}) kept a key: {text}"
            );
            let (status, body) = fx
                .post(
                    &fx.alice,
                    "/api/v0/agents",
                    json!({ "name": format!("{name}-{}", lang.code()), "spec": spec }),
                )
                .await;
            assert_eq!(status, StatusCode::CREATED, "{name} ({lang:?}): {body}");
        }
    }
}
