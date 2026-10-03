use std::sync::Arc;

use serde_json::json;

use super::*;
use aiplane_core::server::principal::{GrantSet, SystemPrincipal};
use aiplane_core::server::run_chain::{CallSite, Frame};

fn principal(id: &str, name: &str) -> SystemPrincipal {
    SystemPrincipal {
        id: id.into(),
        name: name.into(),
        grants: Arc::new(GrantSet::default()),
    }
}

async fn memory() -> Pool {
    aiplane_core::server::db::open(std::path::Path::new(":memory:"))
        .await
        .unwrap()
}

fn main_chain(conversation: &str) -> RunChain {
    RunChain::root(
        conversation,
        Some("v-7".into()),
        Frame::for_principal(&principal("p-main", "support-website"), Some(2)),
    )
}

fn billing_chain(conversation: &str) -> RunChain {
    main_chain(conversation)
        .enter(
            Frame::for_principal(&principal("p-bill", "billing"), Some(5)).called_from(CallSite {
                turn_id: "t-main".into(),
                tool_call_id: "call-1".into(),
            }),
        )
        .unwrap()
}

async fn run_event(pool: &Pool, chain: &RunChain, kind: AuditKind, detail: Value) -> Appended {
    append_now(
        pool,
        NewEvent::new(kind, &chain.current().principal_id, detail)
            .in_run(Some(chain))
            .at(Correlation {
                session_id: Some(chain.root_session.clone()),
                turn_id: Some("t-main".into()),
                round: Some(1),
                call_id: Some("call-1".into()),
                conversation_id: None,
            }),
    )
    .await
    .unwrap()
}

async fn chain_rows(pool: &Pool, key: &str) -> Vec<StoredEvent> {
    let sql = format!("SELECT {COLUMNS} FROM agent_audit WHERE chain_key = ? ORDER BY seq");
    sqlx::query(&sql)
        .bind(key)
        .fetch_all(pool)
        .await
        .unwrap()
        .iter()
        .map(|r| stored(r).unwrap())
        .collect()
}

#[tokio::test]
async fn a_sub_agents_event_lands_in_the_conversations_chain_with_every_correlation_id() {
    let pool = memory().await;
    let chain = billing_chain("s-visitor");

    run_event(
        &pool,
        &chain,
        AuditKind::ToolCall,
        json!({"tool": "lookup_invoice", "decision": "allowed"}),
    )
    .await;

    let rows = chain_rows(&pool, "conversation:s-visitor").await;
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.principal_id, "p-bill");
    assert_eq!(row.agent_id.as_deref(), Some("p-main"));
    assert_eq!(row.version, Some(5));
    assert_eq!(row.conversation_id.as_deref(), Some("s-visitor"));
    assert_eq!(row.visitor_id.as_deref(), Some("v-7"));
    assert_eq!(row.round, Some(1));
    assert_eq!(row.call_id.as_deref(), Some("call-1"));
    assert_eq!(row.seq, Some(1));
    assert_eq!(row.prev_hash, None);
    assert_eq!(row.hash.as_deref(), Some(row.computed_hash().as_str()));
    assert!(for_principal(&pool, "p-main").await.unwrap().is_empty());
    let events = for_principal(&pool, "p-bill").await.unwrap();
    assert_eq!(events[0].chain, Some(chain.to_json()));
}

#[tokio::test]
async fn a_management_event_extends_its_agents_own_chain_on_the_callers_transaction() {
    let pool = memory().await;
    for n in 0..3 {
        let mut tx = pool.begin().await.unwrap();
        record(
            &mut tx,
            AuditKind::GrantAdded,
            "p1",
            "alice",
            json!({"kind": "tool", "n": n}),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    let mut tx = pool.begin().await.unwrap();
    record(&mut tx, AuditKind::GrantAdded, "p1", "alice", json!({}))
        .await
        .unwrap();
    tx.rollback().await.unwrap();

    let rows = chain_rows(&pool, "agent:p1").await;
    assert_eq!(
        rows.iter().map(|r| r.seq.unwrap()).collect::<Vec<_>>(),
        [1, 2, 3],
        "a rolled-back change leaves no event"
    );
    assert_eq!(rows[1].prev_hash, rows[0].hash);
    assert_eq!(rows[2].prev_hash, rows[1].hash);
    assert_eq!(rows[0].actor_id.as_deref(), Some("alice"));
    assert_eq!(rows[0].chain, None);
    let v = verify(&pool, "p1").await.unwrap();
    assert!(v.ok(), "{v:?}");
    assert_eq!((v.chains, v.events), (1, 3));
}

#[tokio::test]
async fn verify_names_the_first_link_that_was_changed_removed_or_never_chained() {
    let pool = memory().await;
    let chain = main_chain("s1");
    for n in 0..4 {
        run_event(&pool, &chain, AuditKind::RouteDecision, json!({"n": n})).await;
    }
    assert!(verify(&pool, "p-main").await.unwrap().ok());

    sqlx::query(
        "UPDATE agent_audit SET detail = '{\"n\":99}' WHERE chain_key = 'conversation:s1' AND seq = 3",
    )
    .execute(&pool)
    .await
    .unwrap();
    let broken = verify(&pool, "p-main").await.unwrap().broken.unwrap();
    assert_eq!(
        (broken.chain_key.as_str(), broken.seq),
        ("conversation:s1", 3)
    );
    assert!(
        broken.reason.contains("does not match its hash"),
        "{broken:?}"
    );

    sqlx::query("DELETE FROM agent_audit WHERE chain_key = 'conversation:s1' AND seq = 2")
        .execute(&pool)
        .await
        .unwrap();
    let broken = verify(&pool, "p-main").await.unwrap().broken.unwrap();
    assert_eq!(broken.seq, 2);
    assert!(broken.reason.contains("missing"), "{broken:?}");

    sqlx::query(
        "INSERT INTO agent_audit (id, kind, principal_id, detail, created_at, agent_id)
         VALUES ('old', 'tool_call', 'p-main', '{}', '2026-01-01T00:00:00Z', 'p-main')",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(verify(&pool, "p-main").await.unwrap().unchained, 1);
}

#[tokio::test]
async fn a_relinked_event_breaks_the_chain_at_the_link_it_skipped() {
    let pool = memory().await;
    let chain = main_chain("s1");
    let first = run_event(&pool, &chain, AuditKind::TurnStarted, json!({})).await;
    run_event(&pool, &chain, AuditKind::TurnFinished, json!({})).await;
    sqlx::query(
        "UPDATE agent_audit SET prev_hash = 'forged' WHERE chain_key = 'conversation:s1' AND seq = 2",
    )
    .execute(&pool)
    .await
    .unwrap();
    let broken = verify(&pool, "p-main").await.unwrap().broken.unwrap();
    assert_eq!(broken.seq, 2);
    assert!(broken.reason.contains("prev_hash"));
    assert_ne!(first.hash, "forged");
}

#[tokio::test]
async fn parallel_writers_to_one_conversation_lose_no_event_and_never_fork_the_chain() {
    let dir = tempfile::tempdir().unwrap();
    let pool = aiplane_core::server::db::open(&dir.path().join("log.sqlite"))
        .await
        .unwrap();
    let writers: Vec<_> = (0..40)
        .map(|n| {
            let pool = pool.clone();
            tokio::spawn(async move {
                let chain = if n % 2 == 0 {
                    main_chain("s-busy")
                } else {
                    billing_chain("s-busy")
                };
                run_event(&pool, &chain, AuditKind::ToolResult, json!({"n": n})).await
            })
        })
        .collect();
    for w in writers {
        w.await.unwrap();
    }
    let rows = chain_rows(&pool, "conversation:s-busy").await;
    assert_eq!(
        rows.iter().map(|r| r.seq.unwrap()).collect::<Vec<_>>(),
        (1..=40).collect::<Vec<_>>()
    );
    let mut ns: Vec<i64> = rows
        .iter()
        .map(|r| {
            serde_json::from_str::<Value>(&r.detail).unwrap()["n"]
                .as_i64()
                .unwrap()
        })
        .collect();
    ns.sort();
    assert_eq!(ns, (0..40).collect::<Vec<_>>());
    let v = verify(&pool, "p-main").await.unwrap();
    assert!(v.ok(), "{v:?}");
    assert_eq!(v.events, 40);
}

#[tokio::test]
async fn a_page_follows_its_filters_and_cursor_and_stops_at_its_byte_cap() {
    let pool = memory().await;
    let a = main_chain("s-a");
    let b = main_chain("s-b");
    for n in 0..5 {
        run_event(&pool, &a, AuditKind::LlmExchange, json!({"n": n})).await;
    }
    run_event(
        &pool,
        &b,
        AuditKind::ToolResult,
        json!({"big": "x".repeat(10_000)}),
    )
    .await;
    run_event(
        &pool,
        &b,
        AuditKind::ToolResult,
        json!({"big": "y".repeat(10_000)}),
    )
    .await;

    let q = ActivityQuery {
        agent_id: "p-main".into(),
        conversation_id: Some("s-a".into()),
        order: Order::Asc,
        limit: 2,
        max_bytes: usize::MAX,
        ..ActivityQuery::default()
    };
    let first = page(&pool, &q).await.unwrap();
    assert_eq!(first.events.len(), 2);
    let second = page(
        &pool,
        &ActivityQuery {
            cursor: first.next_cursor,
            ..q.clone()
        },
    )
    .await
    .unwrap();
    let ns: Vec<String> = first
        .events
        .iter()
        .chain(&second.events)
        .map(|e| e.detail.clone())
        .collect();
    assert_eq!(ns, ["{\"n\":0}", "{\"n\":1}", "{\"n\":2}", "{\"n\":3}"]);

    let capped = page(
        &pool,
        &ActivityQuery {
            agent_id: "p-main".into(),
            kinds: vec![AuditKind::ToolResult],
            limit: 50,
            max_bytes: 5_000,
            ..ActivityQuery::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(capped.events.len(), 1, "one event always fits");
    assert!(capped.events[0].detail.contains('y'), "newest first");
    assert!(capped.next_cursor.is_some());

    assert!(
        page(
            &pool,
            &ActivityQuery {
                agent_id: "p-other".into(),
                limit: 50,
                max_bytes: usize::MAX,
                ..ActivityQuery::default()
            },
        )
        .await
        .unwrap()
        .events
        .is_empty()
    );
}

#[tokio::test]
async fn the_decision_trail_leaves_the_content_events_out() {
    let pool = memory().await;
    let chain = main_chain("s1");
    run_event(&pool, &chain, AuditKind::LlmExchange, json!({})).await;
    run_event(&pool, &chain, AuditKind::ToolCall, json!({})).await;
    let kinds: Vec<String> = for_principal(&pool, "p-main")
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(kinds, ["tool_call"]);
}

#[tokio::test]
async fn the_sweep_takes_whole_chains_of_gone_conversations_only() {
    let pool = memory().await;
    let agent = crate::db::agents::create(
        &pool,
        &crate::db::system_principals::NewPrincipal {
            name: "support",
            display: "Support",
            description: "",
        },
        "{}",
        "alice",
    )
    .await
    .unwrap()
    .unwrap()
    .principal;
    let live = crate::db::run_sessions::create_principal_session(
        &pool,
        &crate::db::run_sessions::NewRunSession {
            principal_id: &agent.id,
            title: None,
            parent_turn_id: None,
            agent_version: Some(1),
        },
    )
    .await
    .unwrap()
    .id;
    let chain_of = |conversation: &str| {
        RunChain::root(
            conversation,
            None,
            Frame::for_principal(&principal(&agent.id, &agent.name), Some(1)),
        )
    };
    let (gone, kept) = (chain_of("s-gone"), chain_of(&live));
    for _ in 0..3 {
        run_event(&pool, &gone, AuditKind::ToolCall, json!({})).await;
        run_event(&pool, &kept, AuditKind::ToolCall, json!({})).await;
    }
    let agent_events = chain_rows(&pool, &agent_chain(&agent.id)).await.len();
    assert!(
        agent_events >= 1,
        "creating the agent is an event of its own chain"
    );

    let early = sweep_conversation_chains(&pool, &agent.id, Timestamp::UNIX_EPOCH)
        .await
        .unwrap();
    assert_eq!(early, SweptChains::default(), "nothing is old enough yet");

    let later = Timestamp::now() + jiff::SignedDuration::from_secs(60);
    let swept = sweep_conversation_chains(&pool, &agent.id, later)
        .await
        .unwrap();
    assert_eq!(
        swept,
        SweptChains {
            chains: 1,
            events: 3
        }
    );
    assert!(chain_rows(&pool, "conversation:s-gone").await.is_empty());
    assert_eq!(
        chain_rows(&pool, &conversation_chain(&live)).await.len(),
        3,
        "a conversation that still exists keeps its log"
    );
    assert_eq!(
        chain_rows(&pool, &agent_chain(&agent.id)).await.len(),
        agent_events,
        "the agent's own chain is never swept"
    );
    assert!(verify(&pool, &agent.id).await.unwrap().ok());
}

#[tokio::test]
async fn the_head_and_the_sweep_never_read_a_row_behind_its_payload() {
    let pool = memory().await;
    for sql in [HEAD_SQL, SWEEP_SQL] {
        let plan: Vec<(i64, i64, i64, String)> =
            sqlx::query_as(&format!("EXPLAIN QUERY PLAN {sql}"))
                .bind("x")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert!(
            plan.iter()
                .any(|(_, _, _, step)| step.contains("COVERING INDEX")),
            "{sql}\n{plan:?}"
        );
    }
}

#[test]
fn canonical_json_sorts_keys_at_every_level_without_whitespace() {
    let v = json!({"b": {"z": 1, "a": [{"y": true, "x": null}]}, "a": "é"});
    assert_eq!(
        canonical_json(&v),
        "{\"a\":\"é\",\"b\":{\"a\":[{\"x\":null,\"y\":true}],\"z\":1}}"
    );
}
