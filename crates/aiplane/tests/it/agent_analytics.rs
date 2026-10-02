// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! `GET /api/v0/agents/{id}/analytics` against seeded rows (issue #100,
//! `docs/agents.md` §5): every number is computed by hand from the fixture
//! below, builder test conversations and another agent's rows are present in
//! the database and must not move any of them, and nothing a visitor said
//! comes back.
//!
//! Agent `support` (v1 and v2) and agent `other` live in one database. All
//! seeded rows fall in September 2026.

use rama::http::{Method, StatusCode};
use serde_json::{Value, json};
use sqlx::SqlitePool;

use crate::agents::{Fx, fixture};

const VISITOR_TEXT: &str = "my-card-number-is-4111";
const VISITOR_ID: &str = "visitor-secret-id";

fn chain(agent: &str, version: i64) -> String {
    json!({
        "root_session": "root",
        "visitor_id": VISITOR_ID,
        "frames": [{ "principal_id": agent, "name": "n", "version": version, "via": null }],
    })
    .to_string()
}

async fn session(db: &SqlitePool, id: &str, agent: &str, version: i64, at: &str, user_turns: u32) {
    sqlx::query(
        "INSERT INTO chat_sessions (id, principal_id, agent_version, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(agent)
    .bind(version)
    .bind(at)
    .bind(at)
    .execute(db)
    .await
    .unwrap();
    for seq in 0..user_turns {
        for (offset, role) in [(0, "user"), (1, "assistant")] {
            sqlx::query(
                "INSERT INTO chat_turns (id, session_id, seq, role, user_content, content, status, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, 'completed', ?)",
            )
            .bind(format!("{id}-{seq}-{role}"))
            .bind(id)
            .bind(i64::from(seq * 2 + offset))
            .bind(role)
            .bind((role == "user").then_some(VISITOR_TEXT))
            .bind((role == "assistant").then_some(VISITOR_TEXT))
            .bind(at)
            .execute(db)
            .await
            .unwrap();
        }
    }
}

async fn audit(
    db: &SqlitePool,
    kind: &str,
    agent: &str,
    chain: Option<String>,
    detail: Value,
    at: &str,
) {
    sqlx::query(
        "INSERT INTO agent_audit (id, kind, principal_id, chain, detail, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(kind)
    .bind(agent)
    .bind(chain)
    .bind(detail.to_string())
    .bind(at)
    .execute(db)
    .await
    .unwrap();
}

async fn usage(
    db: &SqlitePool,
    agent: &str,
    version: i64,
    at: &str,
    tokens: (i64, i64),
    cost: f64,
) {
    sqlx::query(
        "INSERT INTO usage_events (id, created_at, user_id, source, kind, backend, model, status,
                                   duration_ms, prompt_tokens, completion_tokens, total_tokens,
                                   cost, agent_id, chain)
         VALUES (?, ?, 'p', 'chat', 'chat', 'b', 'm', 200, 1, ?, ?, ?, ?, ?, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(at)
    .bind(tokens.0)
    .bind(tokens.1)
    .bind(tokens.0 + tokens.1)
    .bind(cost)
    .bind(agent)
    .bind(chain(agent, version))
    .execute(db)
    .await
    .unwrap();
}

fn closed(route: &str, missing: &[&str]) -> Value {
    json!({ "route": route, "gate": {
        "status": "closed",
        "missing": missing
            .iter()
            .map(|s| json!({"path": "", "slot": s, "kind": "missing", "message": VISITOR_TEXT}))
            .collect::<Vec<_>>(),
    }})
}

fn decision(picked: Option<&str>, reason: Option<&str>, routes: Vec<Value>) -> Value {
    json!({ "routes": routes, "picked": picked, "reason": reason })
}

fn outcome(status: &str, reason: Option<&str>) -> Value {
    let mut o = json!({ "status": status });
    if let Some(kind) = reason {
        o["reason"] = json!({ "kind": kind });
        o["summary"] = json!(VISITOR_TEXT);
    }
    json!({ "route": "billing", "outcome": o })
}

async fn seed(fx: &Fx, a: &str, other: &str) {
    let db = &fx.state.db;
    session(db, "s1", a, 1, "2026-09-10T10:00:00.123456789Z", 2).await;
    session(db, "s2", a, 1, "2026-09-10T12:00:00Z", 1).await;
    session(db, "s3", a, 2, "2026-09-11T09:00:00Z", 3).await;
    session(db, "s-test", a, 0, "2026-09-10T11:00:00Z", 5).await;
    session(db, "s-old", a, 1, "2026-08-01T11:00:00Z", 4).await;
    session(db, "s-other", other, 1, "2026-09-10T11:00:00Z", 7).await;

    let v = |n| Some(chain(a, n));
    let d = |s: &str| format!("2026-09-{s}Z");
    for (picked, version, at) in [
        ("billing", 1, "10T10:00:00"),
        ("billing", 1, "10T10:05:00"),
        ("tech", 2, "11T09:00:00"),
        ("billing", 0, "10T11:00:00"),
    ] {
        let detail = decision(Some(picked), None, vec![]);
        audit(db, "route_decision", a, v(version), detail, &d(at)).await;
    }
    let refusals = [
        (
            vec![
                closed("billing", &["customer_id", "order"]),
                closed("tech", &["issue"]),
            ],
            "10T10:10:00",
        ),
        (vec![closed("billing", &["customer_id"])], "10T10:20:00"),
    ];
    for (routes, at) in refusals {
        let detail = decision(None, Some("no_open_route"), routes);
        audit(db, "route_decision", a, v(1), detail, &d(at)).await;
    }
    let declined = decision(None, Some(VISITOR_TEXT), vec![closed("billing", &["x"])]);
    audit(db, "route_decision", a, v(1), declined, &d("10T10:30:00")).await;

    for (status, reason, version) in [
        ("finished", None, 1),
        ("incomplete", Some("round_budget_exhausted"), 1),
        ("incomplete", Some("failed"), 2),
        ("finished", None, 0),
    ] {
        let at = d("10T10:00:00");
        audit(db, "sub_agent_dispatched", a, v(version), json!({}), &at).await;
        let detail = outcome(status, reason);
        audit(db, "sub_agent_finished", a, v(version), detail, &at).await;
    }
    for (action, version) in [
        ("redacted", 1),
        ("redacted", 2),
        ("withheld", 1),
        ("withheld", 0),
    ] {
        let detail = json!({ "action": action, "patterns": [VISITOR_TEXT] });
        audit(
            db,
            "output_blocked",
            a,
            v(version),
            detail,
            &d("11T10:00:00"),
        )
        .await;
    }
    for limit in ["visitor_rate", "visitor_rate", "budget"] {
        let detail = json!({ "limit": limit, "visitor_id": VISITOR_ID });
        audit(db, "limit_refused", a, None, detail, &d("12T10:00:00")).await;
    }
    audit(db, "human_handoff", a, v(1), json!({}), &d("12T11:00:00")).await;
    audit(db, "tool_call", a, v(1), json!({}), &d("12T11:00:00")).await;
    let after = json!({ "limit": "ip_rate" });
    audit(db, "limit_refused", a, None, after, "2026-10-05T10:00:00Z").await;

    let at = d("12T10:00:00");
    let budget = json!({ "limit": "budget" });
    audit(db, "limit_refused", other, None, budget, &at).await;
    let withheld = json!({ "action": "withheld" });
    audit(
        db,
        "output_blocked",
        other,
        Some(chain(other, 1)),
        withheld,
        &at,
    )
    .await;
    let picked = decision(Some("billing"), None, vec![]);
    audit(
        db,
        "route_decision",
        other,
        Some(chain(other, 1)),
        picked,
        &at,
    )
    .await;

    usage(db, a, 1, &d("10T10:00:00"), (100, 20), 0.5).await;
    usage(db, a, 1, &d("10T10:00:30"), (200, 30), 0.25).await;
    usage(db, a, 2, &d("11T09:00:00"), (10, 5), 0.125).await;
    usage(db, a, 0, &d("10T11:00:00"), (1000, 1000), 9.0).await;
    usage(db, a, 1, "2026-08-01T00:00:00Z", (1000, 1000), 9.0).await;
    usage(db, other, 1, &d("10T10:00:00"), (1000, 1000), 9.0).await;
}

const RANGE: &str = "from=2026-09-01&to=2026-09-30";

async fn setup() -> (Fx, String, String) {
    let fx = fixture().await;
    let a = fx.create(&fx.alice, "support").await;
    let other = fx.create(&fx.alice, "other").await;
    seed(&fx, &a, &other).await;
    (fx, a, other)
}

async fn analytics(fx: &Fx, id: &str, query: &str) -> (StatusCode, Value) {
    fx.get(&fx.alice, &format!("/api/v0/agents/{id}/analytics?{query}"))
        .await
}

#[tokio::test]
async fn the_numbers_match_the_seeded_rows_exactly() {
    let (fx, a, _) = setup().await;
    let (status, body) = analytics(&fx, &a, RANGE).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    assert_eq!(body["conversations"], 3);
    assert_eq!(body["turns"], 6);
    assert_eq!(body["routes_chosen"], json!({ "billing": 2, "tech": 1 }));
    assert_eq!(
        body["sub_agents"],
        json!({
            "dispatched": 3, "finished": 1, "incomplete": 2,
            "incomplete_by_reason": { "failed": 1, "round_budget_exhausted": 1 },
        })
    );
    assert_eq!(body["gate_refusals"]["total"], 2);
    assert_eq!(
        body["gate_refusals"]["by_route"],
        json!({ "billing": 2, "tech": 1 })
    );
    assert_eq!(
        body["gate_refusals"]["by_missing_slot"],
        json!([
            { "route": "billing", "slot": "customer_id", "count": 2 },
            { "route": "billing", "slot": "order", "count": 1 },
            { "route": "tech", "slot": "issue", "count": 1 },
        ])
    );
    assert_eq!(
        body["output_blocks"],
        json!({ "total": 3, "by_action": { "redacted": 2, "withheld": 1 } })
    );
    assert_eq!(
        body["limit_refusals"],
        json!({ "total": 3, "by_kind": { "budget": 1, "visitor_rate": 2 } })
    );
    assert_eq!(body["human_handoffs"], 1);
    assert_eq!(
        body["usage"],
        json!({
            "requests": 3, "prompt_tokens": 310, "completion_tokens": 55,
            "tokens": 365, "cost": 0.875,
        })
    );
}

#[tokio::test]
async fn the_time_series_has_one_bucket_per_day_including_quiet_ones() {
    let (fx, a, _) = setup().await;
    let (_, body) = analytics(&fx, &a, "from=2026-09-09&to=2026-09-12").await;
    let daily = body["daily"].as_array().unwrap();
    let days: Vec<_> = daily.iter().map(|d| d["day"].as_str().unwrap()).collect();
    assert_eq!(
        days,
        ["2026-09-09", "2026-09-10", "2026-09-11", "2026-09-12"]
    );
    assert_eq!(
        daily[1],
        json!({
            "day": "2026-09-10", "conversations": 2, "turns": 3,
            "tokens": 350, "cost": 0.75, "refusals": 2,
        })
    );
    assert_eq!(
        daily[2],
        json!({
            "day": "2026-09-11", "conversations": 1, "turns": 3,
            "tokens": 15, "cost": 0.125, "refusals": 0,
        })
    );
    assert_eq!(daily[0]["conversations"], 0);
    assert_eq!(daily[3]["refusals"], 3);
}

#[tokio::test]
async fn a_version_filter_keeps_that_versions_rows_and_drops_unversioned_refusals() {
    let (fx, a, _) = setup().await;
    let (_, body) = analytics(&fx, &a, &format!("{RANGE}&version=2")).await;
    assert_eq!(body["version"], 2);
    assert_eq!(body["conversations"], 1);
    assert_eq!(body["turns"], 3);
    assert_eq!(body["routes_chosen"], json!({ "tech": 1 }));
    assert_eq!(body["sub_agents"]["dispatched"], 1);
    assert_eq!(
        body["sub_agents"]["incomplete_by_reason"],
        json!({ "failed": 1 })
    );
    assert_eq!(body["output_blocks"]["total"], 1);
    assert_eq!(body["limit_refusals"]["total"], 0);
    assert_eq!(body["human_handoffs"], 0);
    assert_eq!(body["usage"]["tokens"], 15);
}

#[tokio::test]
async fn another_agents_rows_never_leak_and_counts_are_its_own() {
    let (fx, _, other) = setup().await;
    let (_, body) = analytics(&fx, &other, RANGE).await;
    assert_eq!(body["conversations"], 1);
    assert_eq!(body["turns"], 7);
    assert_eq!(body["routes_chosen"], json!({ "billing": 1 }));
    assert_eq!(body["output_blocks"]["total"], 1);
    assert_eq!(body["limit_refusals"]["total"], 1);
    assert_eq!(body["usage"]["tokens"], 2000);
    assert_eq!(body["sub_agents"]["dispatched"], 0);
}

#[tokio::test]
async fn no_visitor_content_comes_back() {
    let (fx, a, _) = setup().await;
    let (_, body) = analytics(&fx, &a, RANGE).await;
    let text = body.to_string();
    assert!(!text.contains(VISITOR_TEXT), "{text}");
    assert!(!text.contains(VISITOR_ID), "{text}");
}

#[tokio::test]
async fn an_agent_without_activity_reports_zeros() {
    let fx = fixture().await;
    let a = fx.create(&fx.alice, "quiet").await;
    let (status, body) = analytics(&fx, &a, RANGE).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["conversations"], 0);
    assert_eq!(body["usage"]["cost"], 0.0);
    assert_eq!(body["daily"].as_array().unwrap().len(), 30);
}

#[tokio::test]
async fn share_rules_apply_like_every_other_agent_route() {
    let (fx, a, _) = setup().await;
    let uri = format!("/api/v0/agents/{a}/analytics?{RANGE}");
    assert_eq!(fx.get(&fx.plain, &uri).await.0, StatusCode::FORBIDDEN);
    assert_eq!(fx.get(&fx.bob, &uri).await.0, StatusCode::NOT_FOUND);
    let shared = fx.share(&fx.alice, &a, "user", "bob", "read").await;
    assert!(shared.0.is_success(), "{shared:?}");
    assert_eq!(fx.get(&fx.bob, &uri).await.0, StatusCode::OK);
    assert_eq!(fx.get(&fx.root, &uri).await.0, StatusCode::OK);
    let anonymous = fx.send(None, Method::GET, &uri, None).await;
    assert_ne!(anonymous.0, StatusCode::OK);
}

#[tokio::test]
async fn a_bad_range_or_version_is_refused() {
    let (fx, a, _) = setup().await;
    for query in [
        "from=2026-09-30&to=2026-09-01",
        "from=yesterday",
        "from=2024-01-01&to=2026-01-01",
        "version=0",
        "version=x",
    ] {
        let (status, body) = analytics(&fx, &a, query).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {body}");
    }
}
