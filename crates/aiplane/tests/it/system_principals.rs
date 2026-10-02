// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! System principals and `gws_` tokens, end to end (issue #77,
//! `docs/agents.md` §1).
//!
//! The fixture is deliberately generous to *people*: a default group grants
//! tools, skills and both MCP connectors to everyone, a global connector is
//! open to all, and two users hold their own OAuth connections. Every test
//! then checks that none of that reaches a system principal unless it was
//! granted one resource at a time, by someone who held it.

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
use aiplane_core::server::db::{self, gateway_groups, mcp_catalog, skill_grants, user_mcp, users};
use aiplane_core::server::principal::{GrantKind, Principal};
use aiplane_core::server::rbac::Resolver;
use aiplane_core::server::upstreams::{
    self,
    config::{PickerStrategy, PoolKind, UpstreamPoolConfig},
};
use aiplane_features::server::skills::{Skill, SkillRegistry, SkillStore, UserSkillStore};
use aiplane_runtime::server::AppState;
use aiplane_runtime::server::tools::ToolRegistry;
use aiplane_runtime::server::tools::echo::Echo;
use aiplane_runtime::server::tools::mcp::manager::AskContext;
use aiplane_runtime::server::tools::time::CurrentTimestamp;
use aiplane_tools::read_skill::ReadSkill;

const TIME: &str = "get_current_timestamp";
const GLOBAL: &str = "globaltools";
const PER_USER: &str = "privdrive";

struct Fixture {
    state: RamaState,
    upstream: MockServer,
    _mcp: MockServer,
    /// MCP calls that carried a person's OAuth credential.
    personal_mcp_hits: Arc<AtomicUsize>,
    admin: String,
    manager: String,
    plain: String,
}

impl Fixture {
    fn app(
        &self,
    ) -> impl Service<Request, Output = rama::http::Response, Error = std::convert::Infallible>
    {
        common::app(self.state.clone())
    }

    async fn send(&self, req: Request) -> (StatusCode, Value) {
        let resp = self.app().serve(req).await.unwrap();
        let status = resp.status();
        let bytes = common::read_body(resp).await;
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn post(&self, cookie: &str, uri: &str, body: Value) -> (StatusCode, Value) {
        self.send(common::post_json(uri, cookie, &body.to_string()))
            .await
    }

    async fn get(&self, cookie: &str, uri: &str) -> (StatusCode, Value) {
        self.send(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .header("cookie", format!("id={cookie}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    async fn create(&self, cookie: &str, name: &str) -> String {
        let (status, body) = self
            .post(cookie, "/api/v0/system-principals", json!({ "name": name }))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["principal"]["id"].as_str().unwrap().to_string()
    }

    async fn grant(&self, cookie: &str, id: &str, kind: &str, r: &str) -> (StatusCode, Value) {
        self.post(
            cookie,
            &format!("/api/v0/system-principals/{id}/grants"),
            json!({ "kind": kind, "ref": r }),
        )
        .await
    }

    async fn token(&self, cookie: &str, id: &str) -> (String, String) {
        let (status, body) = self
            .post(
                cookie,
                &format!("/api/v0/system-principals/{id}/tokens"),
                json!({ "name": "pipeline" }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        (
            body["plaintext"].as_str().unwrap().to_string(),
            body["token"]["id"].as_str().unwrap().to_string(),
        )
    }

    /// A principal that may call the chat pool and nothing else.
    async fn principal_with_pool(&self, name: &str) -> (String, String) {
        let id = self.create(&self.admin, name).await;
        let (status, body) = self.grant(&self.admin, &id, "pool", "pool").await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let (bearer, _) = self.token(&self.admin, &id).await;
        (id, bearer)
    }

    async fn chat(&self, bearer: &str) -> StatusCode {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/chat/completions")
            .header("authorization", format!("Bearer {bearer}"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"model": "model-a", "messages": [{"role": "user", "content": "hi"}]})
                    .to_string(),
            ))
            .unwrap();
        let resp = self.app().serve(req).await.unwrap();
        let status = resp.status();
        let _ = common::read_body(resp).await;
        status
    }

    async fn models_status(&self, bearer: &str) -> StatusCode {
        let req = Request::builder()
            .method(Method::GET)
            .uri("/v1/models")
            .header("authorization", format!("Bearer {bearer}"))
            .body(Body::empty())
            .unwrap();
        self.app().serve(req).await.unwrap().status()
    }

    /// The body of the last request the chat upstream received.
    async fn last_upstream_body(&self) -> Value {
        let requests = self.upstream.received_requests().await.unwrap();
        let last = requests.last().expect("the upstream was called");
        serde_json::from_slice(&last.body).unwrap()
    }

    async fn offered_tools(&self) -> Vec<String> {
        let body = self.last_upstream_body().await;
        body["tools"]
            .as_array()
            .map(|tools| {
                tools
                    .iter()
                    .map(|t| t["function"]["name"].as_str().unwrap().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// An MCP server whose tool list depends on the credential: no credential is
/// the global connector, `alice-secret` / `bob-secret` are two people's own
/// OAuth connections.
async fn mcp_server(personal_hits: Arc<AtomicUsize>) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(move |request: &wiremock::Request| {
            let bearer = request
                .headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let tool = match bearer {
                "" => "shared_lookup",
                "Bearer alice-secret" => "alice_files",
                "Bearer bob-secret" => "bob_files",
                "Bearer erp-secret" => "erp_lookup",
                other => panic!("unexpected MCP credential: {other}"),
            };
            if bearer.contains("alice") || bearer.contains("bob") {
                personal_hits.fetch_add(1, Ordering::SeqCst);
            }
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let result = match body["method"].as_str() {
                Some("initialize") => json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "fixture", "version": "1"},
                }),
                Some("tools/list") => json!({
                    "tools": [{"name": tool, "description": tool,
                        "inputSchema": {"type": "object"},
                        "annotations": {"readOnlyHint": true}}]
                }),
                Some("notifications/initialized") => return ResponseTemplate::new(202),
                other => panic!("unexpected MCP method: {other:?}"),
            };
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({"jsonrpc": "2.0", "id": body["id"], "result": result}))
        })
        .mount(&server)
        .await;
    server
}

fn chat_pool(url: &str, allowed_groups: Vec<String>) -> UpstreamPoolConfig {
    UpstreamPoolConfig {
        voices: Default::default(),
        offer_voices: Vec::new(),
        allowed_groups,
        fallback_offline: None,
        compliance: Default::default(),
        enforce_limits: true,
        kind: PoolKind::Chat,
        strategy: PickerStrategy::RoundRobin,
        models: Vec::new(),
        backend: vec![common::mock_backend("mock", url)],
    }
}

fn connector(key: &str, url: &str, scope: mcp_catalog::Scope) -> mcp_catalog::ConnectorInput {
    mcp_catalog::ConnectorInput {
        key: key.into(),
        name: key.into(),
        description: None,
        icon: None,
        category: None,
        url: url.into(),
        auth: match scope {
            mcp_catalog::Scope::Global | mcp_catalog::Scope::Agent => mcp_catalog::AuthKind::None,
            mcp_catalog::Scope::PerUser => mcp_catalog::AuthKind::OAuth2,
        },
        scope,
        audit: false,
        use_dcr: false,
        client_id: None,
        client_secret_ct: None,
        client_secret_nonce: None,
        authorize_url: None,
        token_url: None,
        registration_url: None,
        scopes: vec![],
        allowed_groups: vec![],
    }
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

async fn fixture() -> Fixture {
    fixture_with_usage(false).await
}

async fn fixture_with_usage(metered: bool) -> Fixture {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5}
        })))
        .mount(&upstream)
        .await;
    let personal_mcp_hits = Arc::new(AtomicUsize::new(0));
    let mcp = mcp_server(personal_mcp_hits.clone()).await;

    let pool = db::open(std::path::Path::new(":memory:")).await.unwrap();
    let mut pools = HashMap::new();
    pools.insert("pool".to_string(), chat_pool(&upstream.uri(), vec![]));
    pools.insert(
        "vip".to_string(),
        chat_pool(&upstream.uri(), vec!["vipgroup".into()]),
    );
    let registry = upstreams::UpstreamRegistry::new(&pools).unwrap();
    common::seed_pool_models(&registry, "pool", 0, &["model-a"]);
    common::seed_pool_models(&registry, "vip", 0, &["model-vip"]);

    let rbac = Arc::new(Resolver::empty());
    let skills = Arc::new(SkillStore::with_registry(
        std::path::PathBuf::from("/nonexistent"),
        SkillRegistry::new([Skill {
            name: "brand".into(),
            title: "Brand".into(),
            description: "Enforce the brand.".into(),
            root: std::path::PathBuf::from("/nonexistent"),
        }]),
    ));
    let user_skills = Arc::new(UserSkillStore::new(std::path::PathBuf::from(
        "/nonexistent/.users",
    )));
    let tools = ToolRegistry::new()
        .with(CurrentTimestamp)
        .with(Echo)
        .with(ReadSkill::new(
            skills.clone(),
            user_skills.clone(),
            rbac.clone(),
        ));
    let app = AppState::new(
        common::test_config(),
        pool.clone(),
        registry,
        Arc::new(tools),
        rbac,
    )
    .with_skills(skills)
    .with_user_skills(user_skills);
    let usage = if metered {
        aiplane_core::server::usage::spawn(pool.clone(), 90)
    } else {
        aiplane_core::server::usage::UsageHandle::disabled()
    };
    let state = RamaState::new(app, SessionStore::new(pool.clone(), TEST_SECRET), usage);

    // Everyone: the time tool, the skill loader, both connectors and every
    // skill. Admins: everything. Managers: agent management, nothing more.
    gateway_groups::upsert_group(&pool, "everyone", "", false, true)
        .await
        .unwrap();
    gateway_groups::set_tools_for_group(
        &pool,
        "everyone",
        &[
            TIME.into(),
            "read_skill".into(),
            format!("mcp__{GLOBAL}"),
            format!("mcp__{PER_USER}"),
        ],
    )
    .await
    .unwrap();
    skill_grants::set_skills_for_role(&pool, "everyone", &["*".into()])
        .await
        .unwrap();
    gateway_groups::upsert_group(&pool, "admins", "", true, false)
        .await
        .unwrap();
    gateway_groups::set_mappings_for_group(&pool, "admins", &["admins".into()])
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
    state.reload_rbac().await;

    mcp_catalog::create(
        &pool,
        connector(GLOBAL, &mcp.uri(), mcp_catalog::Scope::Global),
    )
    .await
    .unwrap();
    mcp_catalog::create(
        &pool,
        connector(PER_USER, &mcp.uri(), mcp_catalog::Scope::PerUser),
    )
    .await
    .unwrap();

    for key in [GLOBAL, PER_USER] {
        mcp_catalog::set_enabled(&pool, key, true).await.unwrap();
    }

    let admin = person(&state, "admin", &["admins"]).await;
    let manager = person(&state, "manager", &["managers"]).await;
    let plain = person(&state, "plain", &[]).await;
    for (user, secret) in [("alice", "alice-secret"), ("bob", "bob-secret")] {
        person(&state, user, &[]).await;
        let sealed = state.crypto.seal_str(secret).unwrap();
        user_mcp::upsert_connection(
            &pool,
            user_mcp::NewConnection {
                user_id: user.into(),
                connector_key: PER_USER.into(),
                access_token_ct: sealed.ciphertext,
                access_token_nonce: sealed.nonce,
                refresh_token_ct: None,
                refresh_token_nonce: None,
                token_expires_at: None,
                scopes: vec![],
                dcr_client_id: None,
                dcr_client_secret_ct: None,
                dcr_client_secret_nonce: None,
                token_url: None,
            },
        )
        .await
        .unwrap();
    }

    Fixture {
        state,
        upstream,
        _mcp: mcp,
        personal_mcp_hits,
        admin,
        manager,
        plain,
    }
}

fn person_principal(id: &str) -> Principal {
    Principal::User {
        id: id.into(),
        roles: vec![],
    }
}

/// The control: on this exact configuration a person *is* offered tools,
/// skills, the global connector and their own connection. Without it the
/// default-deny tests below could pass on an empty gateway.
#[tokio::test]
async fn the_fixture_offers_people_tools_skills_and_connectors() {
    let fx = fixture().await;
    let tools = fx.state.allowed_tools_for_user(&[], "alice").await;
    assert!(tools.contains(&TIME.to_string()), "{tools:?}");
    assert_eq!(fx.state.allowed_skills_for(&[], "alice"), vec!["brand"]);
    let mut mcp = fx
        .state
        .mcp_layer_for(&person_principal("alice"), AskContext::Chat)
        .await
        .tool_ids();
    mcp.sort();
    assert_eq!(
        mcp,
        [
            format!("mcp__{GLOBAL}__shared_lookup"),
            format!("mcp__{PER_USER}__alice_files"),
        ]
    );
}

#[tokio::test]
async fn a_fresh_principal_is_offered_no_tools_connectors_or_skills() {
    let fx = fixture().await;
    let (_, bearer) = fx.principal_with_pool("ci").await;
    let sent = json!([{"role": "user", "content": "hi"}]);

    assert_eq!(fx.chat(&bearer).await, StatusCode::OK);
    let body = fx.last_upstream_body().await;
    assert!(body.get("tools").is_none(), "tools were offered: {body}");
    assert_eq!(
        body["messages"], sent,
        "the request context gained something: {body}"
    );
    assert_eq!(fx.personal_mcp_hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_principal_without_a_pool_grant_cannot_reach_an_open_pool() {
    let fx = fixture().await;
    let id = fx.create(&fx.admin, "ci").await;
    let (bearer, _) = fx.token(&fx.admin, &id).await;
    assert_eq!(fx.chat(&bearer).await, StatusCode::NOT_FOUND);
    assert!(fx.upstream.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn after_granting_one_tool_exactly_that_tool_is_offered() {
    let fx = fixture().await;
    let (id, bearer) = fx.principal_with_pool("ci").await;
    let (status, body) = fx.grant(&fx.manager, &id, "tool", TIME).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    assert_eq!(fx.chat(&bearer).await, StatusCode::OK);
    assert_eq!(fx.offered_tools().await, [TIME]);
}

#[tokio::test]
async fn a_granted_global_connector_never_exposes_a_persons_connection() {
    let fx = fixture().await;
    let (id, bearer) = fx.principal_with_pool("ci").await;
    let (status, body) = fx.grant(&fx.admin, &id, "connector", GLOBAL).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    assert_eq!(fx.chat(&bearer).await, StatusCode::OK);
    assert_eq!(
        fx.offered_tools().await,
        [format!("mcp__{GLOBAL}__shared_lookup")]
    );
    assert_eq!(
        fx.personal_mcp_hits.load(Ordering::SeqCst),
        0,
        "a person's OAuth credential was used for a system principal"
    );
}

#[tokio::test]
async fn a_per_user_connector_cannot_be_granted_and_is_ignored_if_present() {
    let fx = fixture().await;
    let id = fx.create(&fx.admin, "ci").await;
    let (status, body) = fx.grant(&fx.admin, &id, "connector", PER_USER).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains(PER_USER),
        "{body}"
    );

    // Even a grant row that got in some other way reads nobody's connection.
    db::system_principals::add_grant(&fx.state.db, &id, GrantKind::Connector, PER_USER, "x")
        .await
        .unwrap();
    let principal = db::system_principals::load_active(&fx.state.db, &id)
        .await
        .unwrap()
        .unwrap();
    let layer = fx
        .state
        .mcp_layer_for(&Principal::System(principal), AskContext::Chat)
        .await;
    assert!(layer.is_empty(), "{:?}", layer.tool_ids());
    assert_eq!(fx.personal_mcp_hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_grant_beyond_the_managers_own_rights_is_refused_naming_the_resource() {
    let fx = fixture().await;
    let id = fx.create(&fx.manager, "ci").await;
    for (kind, reference) in [("tool", "company_echo"), ("pool", "vip")] {
        let (status, body) = fx.grant(&fx.manager, &id, kind, reference).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{kind} {reference}: {body}");
        assert_eq!(body["error"]["code"], "grant_exceeds_manager");
        let message = body["error"]["message"].as_str().unwrap();
        assert!(message.contains(&format!("`{reference}`")), "{message}");
        assert!(message.contains("Ask an admin"), "{message}");
    }
    let (status, _) = fx.grant(&fx.manager, &id, "tool", "no_such_tool").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = fx.grant(&fx.manager, &id, "tool", "*").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // What the manager does hold goes through.
    let (status, _) = fx.grant(&fx.manager, &id, "skill", "brand").await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = fx.grant(&fx.manager, &id, "pool", "pool").await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, detail) = fx
        .get(&fx.manager, &format!("/api/v0/system-principals/{id}"))
        .await;
    assert_eq!(detail["principal"]["grants"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn a_person_without_agent_management_cannot_create_or_configure() {
    let fx = fixture().await;
    let id = fx.create(&fx.admin, "ci").await;
    let (status, _) = fx
        .post(
            &fx.plain,
            "/api/v0/system-principals",
            json!({"name": "mine"}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = fx.grant(&fx.plain, &id, "pool", "pool").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("can_manage_agents"),
        "{body}"
    );
    for uri in [
        "/api/v0/system-principals".to_string(),
        format!("/api/v0/system-principals/{id}"),
    ] {
        assert_eq!(fx.get(&fx.plain, &uri).await.0, StatusCode::FORBIDDEN);
    }
    for (uri, body) in [
        (
            format!("/api/v0/system-principals/{id}/tokens"),
            json!({"name": "x"}),
        ),
        (format!("/api/v0/system-principals/{id}/disable"), json!({})),
        (
            format!("/api/v0/system-principals/{id}/grants/revoke"),
            json!({"kind": "pool", "ref": "pool"}),
        ),
    ] {
        assert_eq!(
            fx.post(&fx.plain, &uri, body).await.0,
            StatusCode::FORBIDDEN
        );
    }
    let (_, detail) = fx
        .get(&fx.admin, &format!("/api/v0/system-principals/{id}"))
        .await;
    assert!(detail["principal"]["grants"].as_array().unwrap().is_empty());
    assert!(detail["principal"]["tokens"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn every_route_needs_a_session() {
    let fx = fixture().await;
    let routes = [
        (Method::GET, "/api/v0/system-principals"),
        (Method::POST, "/api/v0/system-principals"),
        (Method::GET, "/api/v0/system-principals/p"),
        (Method::POST, "/api/v0/system-principals/p/disable"),
        (Method::POST, "/api/v0/system-principals/p/grants"),
        (Method::POST, "/api/v0/system-principals/p/grants/revoke"),
        (Method::POST, "/api/v0/system-principals/p/tokens"),
        (Method::POST, "/api/v0/system-principals/p/tokens/t/revoke"),
    ];
    for (verb, uri) in routes {
        let (status, _) = fx.send(common::req(verb.clone(), uri)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{verb} {uri}");
    }
}

#[tokio::test]
async fn grants_survive_the_granting_manager_losing_rights_and_leaving() {
    let fx = fixture().await;
    let id = fx.create(&fx.manager, "ci").await;
    assert_eq!(
        fx.grant(&fx.manager, &id, "pool", "pool").await.0,
        StatusCode::CREATED
    );
    assert_eq!(
        fx.grant(&fx.manager, &id, "tool", TIME).await.0,
        StatusCode::CREATED
    );
    let (bearer, _) = fx.token(&fx.manager, &id).await;

    gateway_groups::set_can_manage_agents(&fx.state.db, "managers", false)
        .await
        .unwrap();
    gateway_groups::set_tools_for_group(&fx.state.db, "everyone", &[])
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id = 'manager'")
        .execute(&fx.state.db)
        .await
        .unwrap();
    fx.state.reload_rbac().await;

    assert_eq!(fx.chat(&bearer).await, StatusCode::OK);
    assert_eq!(fx.offered_tools().await, [TIME]);
}

#[tokio::test]
async fn every_grant_change_is_audited_with_who_what_and_when() {
    let fx = fixture().await;
    let id = fx.create(&fx.admin, "ci").await;
    fx.grant(&fx.manager, &id, "tool", TIME).await;
    let (status, _) = fx
        .post(
            &fx.admin,
            &format!("/api/v0/system-principals/{id}/grants/revoke"),
            json!({"kind": "tool", "ref": TIME}),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = fx
        .post(
            &fx.admin,
            &format!("/api/v0/system-principals/{id}/grants/revoke"),
            json!({"kind": "tool", "ref": TIME}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (_, detail) = fx
        .get(&fx.admin, &format!("/api/v0/system-principals/{id}"))
        .await;
    let audit: Vec<(String, String, Value)> = detail["principal"]["audit"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .map(|e| {
            assert!(e["created_at"].is_string(), "{e}");
            (
                e["kind"].as_str().unwrap().to_string(),
                e["actor_id"].as_str().unwrap().to_string(),
                e["detail"].clone(),
            )
        })
        .collect();
    assert_eq!(
        audit,
        [
            (
                "principal_created".into(),
                "admin".into(),
                json!({"name": "ci"})
            ),
            (
                "grant_added".into(),
                "manager".into(),
                json!({"kind": "tool", "ref": TIME})
            ),
            (
                "grant_removed".into(),
                "admin".into(),
                json!({"kind": "tool", "ref": TIME})
            ),
        ]
    );
}

/// A headless run, as the scheduler or (later) an agent dispatch starts one.
async fn headless_run(
    fx: &Fixture,
    owner: aiplane_runtime::server::headless::Owner<'_>,
    principal: Principal,
    run: Option<Arc<aiplane_core::server::run_chain::RunChain>>,
) -> Value {
    use aiplane_runtime::server::headless::{self, DriveParams, OpenParams};
    let (session_id, turn_id) = headless::open_session(
        &fx.state.db,
        OpenParams {
            owner,
            title: "run",
            prompt: "what do you know about me?",
            model: "model-a",
            existing_session: None,
        },
    )
    .await
    .unwrap();
    headless::drive(
        &Arc::new(fx.state.clone()),
        DriveParams {
            principal,
            run,
            session_id,
            assistant_turn_id: turn_id,
            model: "model-a".into(),
            source: aiplane_core::server::db::usage::UsageSource::Scheduled,
            history_limit: None,
            finish: None,
            budget: None,
            injection: Default::default(),
        },
    )
    .await;
    fx.last_upstream_body().await
}

#[tokio::test]
async fn an_agent_run_gets_none_of_its_owners_connectors_memory_or_skills() {
    use aiplane_core::server::run_chain::{Frame, RunChain};
    use aiplane_runtime::server::headless::Owner;
    let fx = fixture().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n",
            "text/event-stream",
        ))
        .with_priority(1)
        .mount(&fx.upstream)
        .await;
    db::user_memories::insert(
        &fx.state.db,
        "alice",
        db::user_memories::MemoryKind::Preference,
        "always answer in pirate speak",
    )
    .await
    .unwrap();

    let persons = headless_run(&fx, Owner::User("alice"), person_principal("alice"), None)
        .await
        .to_string();
    assert!(persons.contains("alice@example.com"), "{persons}");
    assert!(persons.contains("brand"), "{persons}");
    let personal_hits_before = fx.personal_mcp_hits.load(Ordering::SeqCst);

    let (id, _) = fx.principal_with_pool("support-website").await;
    let (status, body) = fx.grant(&fx.admin, &id, "tool", TIME).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let principal = db::system_principals::load_active(&fx.state.db, &id)
        .await
        .unwrap()
        .unwrap();
    let chain = Arc::new(RunChain::root(
        "s-visitor",
        None,
        Frame::for_principal(&principal, Some(1)),
    ));
    let agents = headless_run(
        &fx,
        Owner::Run {
            principal_id: &id,
            parent_turn_id: None,
            agent_version: Some(1),
        },
        Principal::System(principal),
        Some(chain),
    )
    .await;

    assert_eq!(fx.offered_tools().await, [TIME]);
    let agents = agents.to_string();
    for owners in ["alice", "pirate", "brand", PER_USER] {
        assert!(
            !agents.contains(owners),
            "`{owners}` reached the agent run: {agents}"
        );
    }
    assert_eq!(
        fx.personal_mcp_hits.load(Ordering::SeqCst),
        personal_hits_before,
        "the owner's own MCP connection was used for the agent run"
    );
}

#[tokio::test]
async fn the_audit_trail_shows_a_run_events_call_chain() {
    use aiplane_core::server::db::agent_audit::{self, AuditKind};
    use aiplane_core::server::run_chain::{Frame, RunChain};
    let fx = fixture().await;
    let id = fx.create(&fx.admin, "support-website").await;
    let principal = db::system_principals::load_active(&fx.state.db, &id)
        .await
        .unwrap()
        .unwrap();
    let chain = RunChain::root(
        "s-visitor",
        Some("v-1".into()),
        Frame::for_principal(&principal, Some(3)),
    );
    agent_audit::record_run_event(
        &fx.state.db,
        AuditKind::ToolCall,
        &id,
        Some(&chain),
        json!({"tool": TIME, "decision": "denied", "policy": "not_granted"}),
    )
    .await
    .unwrap();

    let (_, detail) = fx
        .get(&fx.admin, &format!("/api/v0/system-principals/{id}"))
        .await;
    let audit = detail["principal"]["audit"].as_array().unwrap();
    let run = audit
        .iter()
        .find(|e| e["kind"] == "tool_call")
        .unwrap_or_else(|| panic!("no run event in {audit:?}"));
    assert_eq!(run["chain"], chain.to_json());
    assert_eq!(run["actor_id"], Value::Null);
    assert_eq!(run["detail"]["policy"], "not_granted");
    let created = audit
        .iter()
        .find(|e| e["kind"] == "principal_created")
        .unwrap();
    assert_eq!(created["chain"], Value::Null);
}

#[tokio::test]
async fn revoking_a_token_or_disabling_the_principal_cuts_it_off() {
    let fx = fixture().await;
    let id = fx.create(&fx.admin, "ci").await;
    let (first, first_id) = fx.token(&fx.admin, &id).await;
    let (second, _) = fx.token(&fx.admin, &id).await;
    assert!(first.starts_with("gws_"));
    assert_eq!(fx.models_status(&first).await, StatusCode::OK);

    let (status, _) = fx
        .post(
            &fx.admin,
            &format!("/api/v0/system-principals/{id}/tokens/{first_id}/revoke"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(fx.models_status(&first).await, StatusCode::UNAUTHORIZED);
    assert_eq!(fx.models_status(&second).await, StatusCode::OK);

    let (status, body) = fx
        .post(
            &fx.admin,
            &format!("/api/v0/system-principals/{id}/disable"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["principal"]["disabled_at"].is_string(), "{body}");
    assert_eq!(fx.models_status(&second).await, StatusCode::UNAUTHORIZED);

    let (status, _) = fx
        .post(
            &fx.admin,
            &format!("/api/v0/system-principals/{id}/tokens"),
            json!({"name": "again"}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, detail) = fx
        .get(&fx.admin, &format!("/api/v0/system-principals/{id}"))
        .await;
    let tokens = detail["principal"]["tokens"].as_array().unwrap();
    assert!(tokens.iter().all(|t| t["revoked"] == true), "{detail}");
    assert!(
        tokens
            .iter()
            .all(|t| t.get("hash").is_none() && t.get("plaintext").is_none()),
        "{detail}"
    );
}

#[tokio::test]
async fn a_system_token_is_never_accepted_as_a_user_token_or_the_other_way() {
    let fx = fixture().await;
    let id = fx.create(&fx.admin, "ci").await;
    let (system, _) = fx.token(&fx.admin, &id).await;
    let user = common::seed_user_with_token(&fx.state, "alice").await;
    let forged_system = system.replacen("gws_", "gwk_", 1);
    let forged_user = user.replacen("gwk_", "gws_", 1);
    assert_eq!(
        fx.models_status(&forged_system).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        fx.models_status(&forged_user).await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn usage_rows_name_the_principal_not_a_user() {
    let fx = fixture_with_usage(true).await;
    let (id, bearer) = fx.principal_with_pool("ci").await;
    assert_eq!(fx.chat(&bearer).await, StatusCode::OK);
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;

    let rows: Vec<(String, String, Option<String>)> =
        sqlx::query_as("SELECT user_id, principal_kind, user_email FROM usage_events")
            .fetch_all(&fx.state.db)
            .await
            .unwrap();
    assert_eq!(rows, [(id, "system".to_string(), Some("ci".to_string()))]);
}

#[tokio::test]
async fn names_are_slugs_and_unique() {
    let fx = fixture().await;
    fx.create(&fx.admin, "support-website").await;
    let (status, body) = fx
        .post(
            &fx.admin,
            "/api/v0/system-principals",
            json!({"name": "support-website"}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (status, _) = fx
        .post(
            &fx.admin,
            "/api/v0/system-principals",
            json!({"name": "Not A Slug"}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, body) = fx.get(&fx.manager, "/api/v0/system-principals").await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = body["principals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["support-website"]);
}

#[tokio::test]
async fn a_system_principal_lists_only_the_models_of_its_granted_pools() {
    let fx = fixture().await;
    let (_, bearer) = fx.principal_with_pool("ci").await;
    let req = Request::builder()
        .method(Method::GET)
        .uri("/v1/models")
        .header("authorization", format!("Bearer {bearer}"))
        .body(Body::empty())
        .unwrap();
    let (status, body) = fx.send(req).await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["model-a"]);
}

const AGENT: &str = "erp";

/// An `agent` connector created the way an operator would: through the admin
/// API, with a static bearer the gateway sends for every principal.
async fn agent_connector(fx: &Fixture, allowed_groups: &[&str]) {
    let body = json!({
        "key": AGENT,
        "title": "ERP",
        "base_url": fx._mcp.uri(),
        "scope": "agent",
        "auth_type": "static_bearer",
        "client_secret": "erp-secret",
        "groups": allowed_groups,
    });
    let req = Request::builder()
        .method(Method::PUT)
        .uri("/api/v0/admin/connectors")
        .header("cookie", format!("id={}", fx.admin))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (status, body) = fx.send(req).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = fx
        .post(
            &fx.admin,
            &format!("/api/v0/admin/connectors/{AGENT}/toggle"),
            json!({"enabled": true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, listed) = fx.get(&fx.admin, "/api/v0/admin/connectors").await;
    let scope = listed["connectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["key"] == AGENT)
        .map(|c| c["scope"].clone());
    assert_eq!(scope, Some(json!("agent")), "{listed}");
}

fn agent_tool() -> String {
    format!("mcp__{AGENT}__erp_lookup")
}

#[tokio::test]
async fn an_agent_connector_cannot_use_per_user_oauth() {
    let fx = fixture().await;
    let body = json!({
        "key": "erp2", "base_url": "http://erp/mcp", "scope": "agent", "auth_type": "oauth2",
    });
    let req = Request::builder()
        .method(Method::PUT)
        .uri("/api/v0/admin/connectors")
        .header("cookie", format!("id={}", fx.admin))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (status, body) = fx.send(req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]["message"].as_str().unwrap().contains("agent"),
        "{body}"
    );
}

#[tokio::test]
async fn no_person_ever_sees_an_agent_connector_admin_included() {
    let fx = fixture().await;
    agent_connector(&fx, &[]).await;
    gateway_groups::set_tools_for_group(&fx.state.db, "everyone", &[format!("mcp__{AGENT}")])
        .await
        .unwrap();
    fx.state.reload_rbac().await;

    for (who, roles) in [("alice", vec![]), ("admin", vec!["admins".to_string()])] {
        let layer = fx
            .state
            .mcp_layer_for(
                &Principal::User {
                    id: who.into(),
                    roles,
                },
                AskContext::Chat,
            )
            .await;
        assert!(
            !layer.tool_ids().iter().any(|id| id.contains(AGENT)),
            "{who} was offered {:?}",
            layer.tool_ids()
        );
    }
    for cookie in [&fx.plain, &fx.admin] {
        let (status, body) = fx.get(cookie, "/api/v0/integrations").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body["connectors"]
                .as_array()
                .unwrap()
                .iter()
                .all(|c| c["key"] != AGENT),
            "{body}"
        );
    }
    let (_, groups) = fx.get(&fx.admin, "/api/v0/admin/groups").await;
    assert!(
        groups["tool_families"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["id"] != format!("mcp__{AGENT}")),
        "{groups}"
    );
}

#[tokio::test]
async fn a_granted_principal_uses_an_agent_connector_an_ungranted_one_does_not() {
    let fx = fixture().await;
    agent_connector(&fx, &[]).await;
    let (granted, granted_bearer) = fx.principal_with_pool("granted").await;
    let (_, other_bearer) = fx.principal_with_pool("other").await;
    let (status, body) = fx.grant(&fx.manager, &granted, "connector", AGENT).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    assert_eq!(fx.chat(&granted_bearer).await, StatusCode::OK);
    assert_eq!(fx.offered_tools().await, [agent_tool()]);

    assert_eq!(fx.chat(&other_bearer).await, StatusCode::OK);
    assert!(fx.offered_tools().await.is_empty());
}

#[tokio::test]
async fn an_agent_connectors_groups_decide_who_may_grant_it() {
    let fx = fixture().await;
    agent_connector(&fx, &["admins"]).await;
    let id = fx.create(&fx.manager, "ci").await;
    let (status, body) = fx.grant(&fx.manager, &id, "connector", AGENT).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "grant_exceeds_manager");
    let (status, _) = fx.grant(&fx.admin, &id, "connector", AGENT).await;
    assert_eq!(status, StatusCode::CREATED);
}
