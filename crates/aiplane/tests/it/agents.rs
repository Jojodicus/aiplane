// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Agent definitions end to end (issue #84, `docs/agents.md` §2): drafts,
//! versions with a live pointer, shares, and spec validation against the
//! agent principal's grants.
//!
//! Two managers (`alice`, `bob`) hold `can_manage_agents` through the
//! `managers` group; `plain` holds nothing but the default group, which grants
//! the time tool and leaves the pool open to everyone — so every manager holds
//! both and can grant them to an agent. `root` is an admin.

use std::collections::HashMap;
use std::sync::Arc;

use rama::Service;
use rama::http::{Body, Method, Request, StatusCode};
use serde_json::{Value, json};

use crate::common::{self, TEST_SECRET};

use aiplane::rama_server::{RamaState, SessionStore};
use aiplane_core::server::db::{self, agent_audit, gateway_groups, users};
use aiplane_core::server::rbac::Resolver;
use aiplane_core::server::upstreams::{
    self,
    config::{PickerStrategy, PoolKind, UpstreamPoolConfig},
};
use aiplane_runtime::server::AppState;
use aiplane_runtime::server::tools::ToolRegistry;
use aiplane_runtime::server::tools::time::CurrentTimestamp;

pub(crate) const TIME: &str = "get_current_timestamp";

pub(crate) struct Fx {
    pub(crate) state: RamaState,
    pub(crate) root: String,
    pub(crate) alice: String,
    pub(crate) bob: String,
    pub(crate) plain: String,
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

pub(crate) async fn fixture() -> Fx {
    let pool = db::open(std::path::Path::new(":memory:")).await.unwrap();
    let mut pools = HashMap::new();
    pools.insert(
        "pool".to_string(),
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
            backend: vec![common::mock_backend("mock", "http://127.0.0.1:9")],
        },
    );
    let registry = upstreams::UpstreamRegistry::new(&pools).unwrap();
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
    gateway_groups::set_tools_for_group(&pool, "everyone", &[TIME.into()])
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
    gateway_groups::upsert_group(&pool, "admins", "", true, false)
        .await
        .unwrap();
    gateway_groups::set_mappings_for_group(&pool, "admins", &["admins".into()])
        .await
        .unwrap();
    gateway_groups::upsert_group(&pool, "support", "", false, false)
        .await
        .unwrap();
    state.reload_rbac().await;

    let root = person(&state, "root", &["admins"]).await;
    let alice = person(&state, "alice", &["managers"]).await;
    let bob = person(&state, "bob", &["managers"]).await;
    let plain = person(&state, "plain", &[]).await;
    Fx {
        state,
        root,
        alice,
        bob,
        plain,
    }
}

impl Fx {
    pub(crate) async fn send(
        &self,
        cookie: Option<&str>,
        method: Method,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut req = Request::builder().method(method).uri(uri);
        if let Some(c) = cookie {
            req = req.header("cookie", format!("id={c}"));
        }
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

    pub(crate) async fn get(&self, cookie: &str, uri: &str) -> (StatusCode, Value) {
        self.send(Some(cookie), Method::GET, uri, None).await
    }

    pub(crate) async fn post(&self, cookie: &str, uri: &str, body: Value) -> (StatusCode, Value) {
        self.send(Some(cookie), Method::POST, uri, Some(body)).await
    }

    pub(crate) async fn put_draft(
        &self,
        cookie: &str,
        id: &str,
        spec: Value,
    ) -> (StatusCode, Value) {
        self.send(
            Some(cookie),
            Method::PUT,
            &format!("/api/v0/agents/{id}/draft"),
            Some(json!({ "spec": spec })),
        )
        .await
    }

    pub(crate) async fn create(&self, cookie: &str, name: &str) -> String {
        let (status, body) = self
            .post(cookie, "/api/v0/agents", json!({ "name": name }))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["agent"]["id"].as_str().unwrap().to_string()
    }

    pub(crate) async fn grant(&self, cookie: &str, id: &str, kind: &str, r: &str) -> StatusCode {
        self.post(
            cookie,
            &format!("/api/v0/system-principals/{id}/grants"),
            json!({ "kind": kind, "ref": r }),
        )
        .await
        .0
    }

    pub(crate) async fn publish(&self, cookie: &str, id: &str) -> (StatusCode, Value) {
        self.post(cookie, &format!("/api/v0/agents/{id}/publish"), json!({}))
            .await
    }

    pub(crate) async fn share(
        &self,
        cookie: &str,
        id: &str,
        kind: &str,
        subject: &str,
        access: &str,
    ) -> (StatusCode, Value) {
        self.post(
            cookie,
            &format!("/api/v0/agents/{id}/shares"),
            json!({ "subject_kind": kind, "subject_id": subject, "access": access }),
        )
        .await
    }

    /// An agent with the pool and the time tool granted and a publishable
    /// draft that uses both.
    pub(crate) async fn runnable(&self, name: &str) -> String {
        let id = self.create(&self.alice, name).await;
        assert_eq!(
            self.grant(&self.alice, &id, "pool", "pool").await,
            StatusCode::CREATED
        );
        assert_eq!(
            self.grant(&self.alice, &id, "tool", TIME).await,
            StatusCode::CREATED
        );
        let (status, body) = self.put_draft(&self.alice, &id, spec("v1")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        id
    }

    pub(crate) async fn audit_kinds(&self, id: &str) -> Vec<String> {
        agent_audit::for_principal(&self.state.db, id)
            .await
            .unwrap()
            .into_iter()
            .rev()
            .map(|e| e.kind)
            .collect()
    }
}

pub(crate) fn spec(orchestration: &str) -> Value {
    json!({
        "main": {
            "pool": "pool",
            "instructions": { "orchestration": orchestration },
            "tools": [TIME]
        }
    })
}

#[tokio::test]
async fn every_agent_route_needs_a_session() {
    let fx = fixture().await;
    for (method, uri) in [
        (Method::GET, "/api/v0/agents"),
        (Method::POST, "/api/v0/agents"),
        (Method::GET, "/api/v0/agents/x"),
        (Method::DELETE, "/api/v0/agents/x"),
        (Method::PUT, "/api/v0/agents/x/draft"),
        (Method::POST, "/api/v0/agents/x/publish"),
        (Method::GET, "/api/v0/agents/x/versions"),
        (Method::POST, "/api/v0/agents/x/live"),
        (Method::GET, "/api/v0/agents/x/shares"),
        (Method::POST, "/api/v0/agents/x/shares"),
        (Method::POST, "/api/v0/agents/x/shares/revoke"),
    ] {
        let (status, _) = fx.send(None, method.clone(), uri, Some(json!({}))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
}

#[tokio::test]
async fn a_non_manager_can_neither_create_list_read_nor_share() {
    let fx = fixture().await;
    let id = fx.create(&fx.alice, "support").await;
    let (status, body) = fx
        .post(&fx.plain, "/api/v0/agents", json!({ "name": "mine" }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("can_manage_agents"),
        "{body}"
    );
    assert_eq!(
        fx.get(&fx.plain, "/api/v0/agents").await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        fx.get(&fx.plain, &format!("/api/v0/agents/{id}")).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        fx.share(&fx.plain, &id, "user", "plain", "read").await.0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn creating_an_agent_creates_a_principal_without_rights_and_a_write_share() {
    let fx = fixture().await;
    let (status, body) = fx
        .post(
            &fx.alice,
            "/api/v0/agents",
            json!({ "name": "support", "display": "Support", "spec": { "profile": { "display": "Hi" } } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["agent"]["id"].as_str().unwrap();
    assert_eq!(body["agent"]["access"], "write");
    assert_eq!(body["agent"]["live_version"], Value::Null);

    let (_, detail) = fx.get(&fx.alice, &format!("/api/v0/agents/{id}")).await;
    let agent = &detail["agent"];
    assert_eq!(agent["draft_spec"]["profile"]["display"], "Hi");
    assert_eq!(agent["grants"], json!([]));
    assert_eq!(
        agent["shares"],
        json!([{ "subject_kind": "user", "subject_id": "alice", "access": "write" }])
    );
    assert_eq!(agent["publish_issues"][0]["path"], "main");

    let (_, principal) = fx
        .get(&fx.alice, &format!("/api/v0/system-principals/{id}"))
        .await;
    assert_eq!(principal["principal"]["name"], "support");

    let (_, list) = fx.get(&fx.alice, "/api/v0/agents").await;
    assert_eq!(list["agents"].as_array().unwrap().len(), 1);
    assert_eq!(list["agents"][0]["name"], "support");

    let (status, body) = fx
        .post(&fx.alice, "/api/v0/agents", json!({ "name": "support" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (status, _) = fx
        .post(&fx.alice, "/api/v0/agents", json!({ "name": "Not A Slug" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_agent_is_invisible_to_a_manager_without_a_share() {
    let fx = fixture().await;
    let id = fx.create(&fx.alice, "support").await;
    let (_, list) = fx.get(&fx.bob, "/api/v0/agents").await;
    assert_eq!(list["agents"], json!([]));
    for uri in [
        format!("/api/v0/agents/{id}"),
        format!("/api/v0/agents/{id}/versions"),
        format!("/api/v0/agents/{id}/shares"),
        format!("/api/v0/system-principals/{id}"),
    ] {
        assert_eq!(
            fx.get(&fx.bob, &uri).await.0,
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
    assert_eq!(
        fx.grant(&fx.bob, &id, "pool", "pool").await,
        StatusCode::NOT_FOUND
    );
    let (_, principals) = fx.get(&fx.bob, "/api/v0/system-principals").await;
    assert_eq!(principals["principals"], json!([]));
}

#[tokio::test]
async fn a_spec_naming_an_ungranted_tool_is_rejected_with_the_grant_to_make() {
    let fx = fixture().await;
    let id = fx.create(&fx.alice, "support").await;
    let (status, body) = fx.put_draft(&fx.alice, &id, spec("hi")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "invalid_agent_spec");
    let issues = body["error"]["issues"].as_array().unwrap();
    let paths: Vec<&str> = issues.iter().map(|i| i["path"].as_str().unwrap()).collect();
    assert_eq!(paths, ["main.pool", "main.tools[0]"]);
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("pool `pool` is not granted"), "{message}");
    assert!(
        issues[1]["message"].as_str().unwrap().contains(&format!(
            "POST /api/v0/system-principals/{id}/grants {{\"kind\": \"tool\", \"ref\": \"{TIME}\"}}"
        )),
        "{body}"
    );

    let (status, body) = fx
        .put_draft(&fx.alice, &id, json!({ "main": { "tool_resorces": {} } }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["issues"][0]["path"], "main.tool_resorces");

    assert_eq!(
        fx.grant(&fx.alice, &id, "pool", "pool").await,
        StatusCode::CREATED
    );
    assert_eq!(
        fx.grant(&fx.alice, &id, "tool", TIME).await,
        StatusCode::CREATED
    );
    let (status, body) = fx.put_draft(&fx.alice, &id, spec("hi")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn a_grant_revoked_after_saving_blocks_publishing() {
    let fx = fixture().await;
    let id = fx.runnable("support").await;
    let (status, _) = fx
        .post(
            &fx.alice,
            &format!("/api/v0/system-principals/{id}/grants/revoke"),
            json!({ "kind": "tool", "ref": TIME }),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = fx.publish(&fx.alice, &id).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["issues"][0]["path"], "main.tools[0]");
}

#[tokio::test]
async fn an_incomplete_draft_saves_but_does_not_publish() {
    let fx = fixture().await;
    let id = fx.create(&fx.alice, "support").await;
    let (status, body) = fx.publish(&fx.alice, &id).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("cannot publish the agent: at `main`"),
        "{body}"
    );
}

#[tokio::test]
async fn editing_the_draft_never_changes_the_live_version() {
    let fx = fixture().await;
    let id = fx.runnable("support").await;
    let (status, body) = fx.publish(&fx.alice, &id).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["version"], 1);

    let (status, body) = fx.put_draft(&fx.alice, &id, spec("v2")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["live_version"], 1);

    let (_, detail) = fx.get(&fx.alice, &format!("/api/v0/agents/{id}")).await;
    assert_eq!(detail["agent"]["live_version"], 1);
    assert_eq!(
        detail["agent"]["live_spec"]["main"]["instructions"]["orchestration"],
        "v1"
    );
    assert_eq!(
        detail["agent"]["draft_spec"]["main"]["instructions"]["orchestration"],
        "v2"
    );
    let (_, versions) = fx
        .get(&fx.alice, &format!("/api/v0/agents/{id}/versions"))
        .await;
    assert_eq!(versions["versions"].as_array().unwrap().len(), 1);
    assert_eq!(
        versions["versions"][0]["spec"]["main"]["instructions"]["orchestration"],
        "v1"
    );
}

#[tokio::test]
async fn publishing_adds_versions_and_rollback_moves_the_live_pointer() {
    let fx = fixture().await;
    let id = fx.runnable("support").await;
    fx.publish(&fx.alice, &id).await;
    fx.put_draft(&fx.alice, &id, spec("v2")).await;
    let (_, body) = fx.publish(&fx.alice, &id).await;
    assert_eq!(body["version"], 2);

    let (status, body) = fx
        .post(
            &fx.alice,
            &format!("/api/v0/agents/{id}/live"),
            json!({ "version": 1 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, detail) = fx.get(&fx.alice, &format!("/api/v0/agents/{id}")).await;
    assert_eq!(detail["agent"]["live_version"], 1);
    assert_eq!(
        detail["agent"]["live_spec"]["main"]["instructions"]["orchestration"],
        "v1"
    );

    let (_, versions) = fx
        .get(&fx.alice, &format!("/api/v0/agents/{id}/versions"))
        .await;
    assert_eq!(versions["live_version"], 1);
    let numbers: Vec<i64> = versions["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["version"].as_i64().unwrap())
        .collect();
    assert_eq!(numbers, [2, 1]);
    assert_eq!(versions["versions"][0]["published_by"], "alice");

    let (status, body) = fx
        .post(
            &fx.alice,
            &format!("/api/v0/agents/{id}/live"),
            json!({ "version": 9 }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn a_share_is_refused_for_anyone_without_the_agent_management_permission() {
    let fx = fixture().await;
    let id = fx.create(&fx.alice, "support").await;
    let (status, body) = fx.share(&fx.alice, &id, "user", "plain", "read").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "share_needs_agent_manager");
    let (status, body) = fx.share(&fx.alice, &id, "group", "support", "read").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        fx.share(&fx.alice, &id, "user", "nobody", "read").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fx.share(&fx.alice, &id, "group", "nogroup", "read").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fx.share(&fx.alice, &id, "user", "bob", "admin").await.0,
        StatusCode::BAD_REQUEST
    );

    let (_, shares) = fx
        .get(&fx.alice, &format!("/api/v0/agents/{id}/shares"))
        .await;
    assert_eq!(shares["shares"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_read_share_shows_the_agent_but_does_not_allow_changing_it() {
    let fx = fixture().await;
    let id = fx.runnable("support").await;
    let (status, body) = fx.share(&fx.alice, &id, "user", "bob", "read").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, detail) = fx.get(&fx.bob, &format!("/api/v0/agents/{id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["agent"]["access"], "read");
    assert_eq!(
        fx.get(&fx.bob, &format!("/api/v0/system-principals/{id}"))
            .await
            .0,
        StatusCode::OK
    );

    let (status, body) = fx.put_draft(&fx.bob, &id, spec("bob was here")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "agent_write_required");
    assert_eq!(fx.publish(&fx.bob, &id).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        fx.share(&fx.bob, &id, "user", "bob", "write").await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        fx.grant(&fx.bob, &id, "pool", "pool").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        fx.send(
            Some(&fx.bob),
            Method::DELETE,
            &format!("/api/v0/agents/{id}"),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );

    let (status, _) = fx
        .post(
            &fx.alice,
            &format!("/api/v0/agents/{id}/shares/revoke"),
            json!({ "subject_kind": "user", "subject_id": "bob" }),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        fx.get(&fx.bob, &format!("/api/v0/agents/{id}")).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_group_share_reaches_every_manager_in_the_group() {
    let fx = fixture().await;
    let id = fx.create(&fx.alice, "support").await;
    let (status, body) = fx.share(&fx.alice, &id, "group", "managers", "write").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (_, list) = fx.get(&fx.bob, "/api/v0/agents").await;
    assert_eq!(list["agents"][0]["access"], "write");
    let (status, body) = fx
        .put_draft(&fx.bob, &id, json!({ "profile": { "display": "Bob" } }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn the_last_write_share_cannot_be_removed() {
    let fx = fixture().await;
    let id = fx.create(&fx.alice, "support").await;
    let (status, body) = fx
        .post(
            &fx.alice,
            &format!("/api/v0/agents/{id}/shares/revoke"),
            json!({ "subject_kind": "user", "subject_id": "alice" }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "last_writer");
    let (status, _) = fx.share(&fx.alice, &id, "user", "alice", "read").await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_route_to_a_missing_sub_agent_is_rejected_and_an_existing_one_accepted() {
    let fx = fixture().await;
    let main = fx.create(&fx.alice, "support").await;
    let sub = fx.create(&fx.alice, "billing").await;
    let draft = |agent: &str| {
        json!({
            "state": { "issue": { "type": "string", "set_by": ["llm"] } },
            "routes": { "billing": {
                "when": { "slot": "issue", "eq": "billing" },
                "agent": agent,
                "task": "{issue}"
            } }
        })
    };
    let (status, body) = fx.put_draft(&fx.alice, &main, draft("billing")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["issues"][0]["path"], "routes.billing.agent");
    let (status, body) = fx.put_draft(&fx.alice, &main, draft(&sub)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn deleting_an_agent_removes_it_and_its_principal_but_not_the_audit_trail() {
    let fx = fixture().await;
    let id = fx.runnable("support").await;
    let (status, _) = fx
        .send(
            Some(&fx.alice),
            Method::DELETE,
            &format!("/api/v0/agents/{id}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        fx.get(&fx.alice, &format!("/api/v0/agents/{id}")).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fx.get(&fx.alice, &format!("/api/v0/system-principals/{id}"))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(fx.audit_kinds(&id).await.last().unwrap(), "agent_deleted");
}

#[tokio::test]
async fn every_mutation_writes_an_audit_row_with_its_actor() {
    let fx = fixture().await;
    let id = fx.runnable("support").await;
    fx.publish(&fx.alice, &id).await;
    fx.post(
        &fx.alice,
        &format!("/api/v0/agents/{id}/live"),
        json!({ "version": 1 }),
    )
    .await;
    fx.share(&fx.alice, &id, "user", "bob", "write").await;
    fx.post(
        &fx.bob,
        &format!("/api/v0/agents/{id}/shares/revoke"),
        json!({ "subject_kind": "user", "subject_id": "alice" }),
    )
    .await;
    assert_eq!(
        fx.audit_kinds(&id).await,
        [
            "principal_created",
            "agent_created",
            "grant_added",
            "grant_added",
            "agent_draft_updated",
            "agent_published",
            "agent_live_version_set",
            "agent_share_set",
            "agent_share_removed",
        ]
    );
    let (_, detail) = fx.get(&fx.bob, &format!("/api/v0/agents/{id}")).await;
    let audit = detail["agent"]["audit"].as_array().unwrap();
    assert_eq!(audit[0]["kind"], "agent_share_removed");
    assert_eq!(audit[0]["actor_id"], "bob");
    assert_eq!(audit[0]["detail"]["subject_id"], "alice");
}

#[tokio::test]
async fn an_admin_holds_write_on_every_agent_without_a_share() {
    let fx = fixture().await;
    let id = fx.runnable("support").await;

    let (_, list) = fx.get(&fx.root, "/api/v0/agents").await;
    assert_eq!(list["agents"][0]["id"], id.as_str());
    assert_eq!(list["agents"][0]["access"], "write");
    let (status, detail) = fx.get(&fx.root, &format!("/api/v0/agents/{id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["agent"]["access"], "write");

    let (status, body) = fx.put_draft(&fx.root, &id, spec("rescued")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(fx.publish(&fx.root, &id).await.0, StatusCode::CREATED);
    let (status, body) = fx.share(&fx.root, &id, "user", "bob", "write").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        fx.grant(&fx.root, &id, "pool", "pool").await,
        StatusCode::OK
    );
    let (status, _) = fx
        .send(
            Some(&fx.root),
            Method::DELETE,
            &format!("/api/v0/agents/{id}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn admin_access_does_not_extend_to_a_manager_without_a_share() {
    let fx = fixture().await;
    let id = fx.runnable("support").await;
    fx.get(&fx.root, &format!("/api/v0/agents/{id}")).await;
    assert_eq!(
        fx.get(&fx.bob, "/api/v0/agents").await.1["agents"],
        json!([])
    );
    assert_eq!(
        fx.get(&fx.bob, &format!("/api/v0/agents/{id}")).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fx.put_draft(&fx.bob, &id, spec("nope")).await.0,
        StatusCode::NOT_FOUND
    );
}
