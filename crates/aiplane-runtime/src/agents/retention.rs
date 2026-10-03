// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The retention sweeper (`docs/agents.md` §5, "What #92 built"): deletes
//! each agent's conversations once they have been idle longer than its live
//! version's `publish.retention_days`, with their sub-agent runs, state and
//! visitor sessions. A person's chat is never touched
//! (`db::agent_retention`). Then the activity log (#111): the whole chain of
//! every conversation that is gone and whose last event is older than
//! `publish.audit_retention_days`.

use std::collections::HashMap;
use std::time::Duration;

use aiplane_agents::db::agent_audit::{self, AuditKind};
use aiplane_agents::db::agent_retention::{self, Swept};
use aiplane_agents::db::agents as agents_db;
use aiplane_core::server::db::{DbError, Pool};
use jiff::{SignedDuration, Timestamp};
use serde_json::json;

use super::spec::AgentSpec;

/// How long an agent keeps a conversation after its last message when its
/// live version sets no `publish.retention_days`, or it has none.
pub const DEFAULT_RETENTION_DAYS: i64 = 30;

/// How long the activity log keeps a conversation's events after its last
/// one, when the live version sets no `publish.audit_retention_days`.
pub const DEFAULT_AUDIT_RETENTION_DAYS: i64 = 365;

/// Retention is counted in days, so sweeping every hour deletes a
/// conversation at most an hour late.
const SWEEP_EVERY: Duration = Duration::from_secs(3_600);

/// One sweep at `now` over every agent. Returns what was deleted per agent
/// id, only for agents where something was; each of those is also audited
/// on the agent, as counts. `now` is a parameter so tests can move past a
/// retention period without waiting for it.
pub async fn sweep(pool: &Pool, now: Timestamp) -> Result<HashMap<String, Swept>, DbError> {
    let live = agents_db::live_specs(pool).await?;
    let mut swept = HashMap::new();
    for agent in agents_db::list_all(pool).await? {
        let id = agent.principal.id;
        let spec = live
            .get(&id)
            .and_then(|s| serde_json::from_str::<AgentSpec>(s).ok());
        let days = spec
            .as_ref()
            .map_or(DEFAULT_RETENTION_DAYS, |s| s.publish.retention_days());
        let audit_days = spec.as_ref().map_or(DEFAULT_AUDIT_RETENTION_DAYS, |s| {
            s.publish.audit_retention_days()
        });
        let gone =
            agent_retention::delete_idle_conversations(pool, &id, days_before(now, days)).await?;
        sweep_activity(pool, &id, audit_days, days_before(now, audit_days)).await?;
        if gone.is_empty() {
            continue;
        }
        super::audit::record(
            pool,
            AuditKind::ConversationsSwept,
            &id,
            None,
            None,
            json!({
                "retention_days": days,
                "conversations": gone.conversations,
                "sub_agent_runs": gone.sub_agent_runs,
            }),
        )
        .await;
        swept.insert(id, gone);
    }
    Ok(swept)
}

fn days_before(now: Timestamp, days: i64) -> Timestamp {
    now.checked_sub(SignedDuration::from_hours(days.saturating_mul(24)))
        .unwrap_or(Timestamp::MIN)
}

/// Delete the activity log's chains of `agent_id`'s conversations that are
/// gone and quiet since `before`, each removal marked on the agent's own
/// chain in the same transaction, after cutting that chain back to `before`
/// behind a checkpoint (`agent_audit::sweep_conversation_chains`).
async fn sweep_activity(
    pool: &Pool,
    agent_id: &str,
    days: i64,
    before: Timestamp,
) -> Result<(), DbError> {
    let swept = agent_audit::sweep_conversation_chains(pool, agent_id, before).await?;
    if swept.chains > 0 || swept.agent_events > 0 {
        tracing::info!(
            agent = agent_id,
            audit_retention_days = days,
            chains = swept.chains,
            events = swept.events,
            agent_events = swept.agent_events,
            "agents: deleted activity log chains past their retention period"
        );
    }
    Ok(())
}

/// Sweep at boot and then every hour. Boot matters: a gateway that was down
/// past a retention deadline deletes on start, not an hour later.
pub fn spawn_retention_sweeper(pool: Pool) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(SWEEP_EVERY);
        loop {
            tick.tick().await;
            match sweep(&pool, Timestamp::now()).await {
                Ok(swept) if !swept.is_empty() => {
                    let conversations: u64 = swept.values().map(|s| s.conversations).sum();
                    tracing::info!(
                        agents = swept.len(),
                        conversations,
                        "agents: deleted conversations past their retention period"
                    );
                }
                Ok(_) => {}
                Err(err) => tracing::warn!(error = %err, "agents: retention sweep failed"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiplane_agents::db::agent_audit::{Correlation, NewEvent};
    use aiplane_agents::db::{run_sessions, system_principals as sp};
    use aiplane_core::server::run_chain::{Frame, RunChain};
    use serde_json::Value;

    const DAY: SignedDuration = SignedDuration::from_hours(24);

    /// An agent keeping conversations one day and their log two, with one
    /// conversation idle since `idle_since` whose log has three events.
    async fn agent_with_a_logged_conversation(
        pool: &Pool,
        idle_since: Timestamp,
    ) -> (String, String) {
        let agent = agents_db::create(
            pool,
            &sp::NewPrincipal {
                name: "support",
                display: "Support",
                description: "",
            },
            "{}",
            "u1",
        )
        .await
        .unwrap()
        .unwrap()
        .principal;
        let spec = json!({ "publish": { "retention_days": 1, "audit_retention_days": 2 } });
        agents_db::publish(pool, &agent.id, &spec.to_string(), "u1")
            .await
            .unwrap()
            .unwrap();
        let session = run_sessions::create_principal_session(
            pool,
            &run_sessions::NewRunSession {
                principal_id: &agent.id,
                title: None,
                parent_turn_id: None,
                agent_version: Some(1),
            },
        )
        .await
        .unwrap()
        .id;
        sqlx::query("UPDATE chat_sessions SET updated_at = ? WHERE id = ?")
            .bind(idle_since.to_string())
            .bind(&session)
            .execute(pool)
            .await
            .unwrap();
        let principal = sp::load_active(pool, &agent.id).await.unwrap().unwrap();
        let chain = RunChain::root(&session, None, Frame::for_principal(&principal, Some(1)));
        for kind in [
            AuditKind::TurnStarted,
            AuditKind::LlmExchange,
            AuditKind::TurnFinished,
        ] {
            super::super::audit::record_event(
                pool,
                NewEvent::new(kind, &agent.id, json!({}))
                    .in_run(Some(&chain))
                    .at(Correlation::default()),
            )
            .await
            .unwrap();
        }
        (agent.id, session)
    }

    async fn chain_len(pool: &Pool, session: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM agent_audit WHERE conversation_id = ?")
            .bind(session)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_gone_conversations_log_is_swept_whole_once_its_own_retention_passed() {
        let pool = aiplane_core::server::db::open(std::path::Path::new(":memory:"))
            .await
            .unwrap();
        let now = Timestamp::now();
        let (agent, session) = agent_with_a_logged_conversation(&pool, now - 2 * DAY).await;

        let swept = sweep(&pool, now + DAY / 2).await.unwrap();
        assert_eq!(swept[&agent].conversations, 1, "the conversation is gone");
        assert_eq!(
            chain_len(&pool, &session).await,
            3,
            "its log outlives it until the log's own retention"
        );

        sweep(&pool, now + 3 * DAY).await.unwrap();
        assert_eq!(
            chain_len(&pool, &session).await,
            0,
            "then the whole chain goes"
        );
        let marker = agent_audit::for_principal(&pool, &agent)
            .await
            .unwrap()
            .into_iter()
            .find(|e| e.kind == "activity_swept")
            .expect("the sweep leaves a marker on the agent's own chain");
        assert_eq!(
            marker.detail["chain_key"],
            format!("conversation:{session}")
        );
        assert_eq!(marker.detail["events"], 3);
        assert!(agent_audit::verify(&pool, &agent).await.unwrap().ok());
    }

    #[tokio::test]
    async fn a_conversation_that_still_exists_keeps_its_log_however_old() {
        let pool = aiplane_core::server::db::open(std::path::Path::new(":memory:"))
            .await
            .unwrap();
        let now = Timestamp::now();
        let (_, session) = agent_with_a_logged_conversation(&pool, now).await;
        sqlx::query("UPDATE agent_audit SET created_at = ? WHERE conversation_id = ?")
            .bind((now - 30 * DAY).to_string())
            .bind(&session)
            .execute(&pool)
            .await
            .unwrap();
        sweep(&pool, now).await.unwrap();
        assert_eq!(chain_len(&pool, &session).await, 3);
    }

    #[test]
    fn retention_comes_from_the_publish_settings_with_a_30_day_default() {
        let days = |spec: Value| AgentSpec::from_value(&spec).map(|s| s.publish.retention_days());
        assert_eq!(
            days(json!({ "publish": { "retention_days": 7 } })).unwrap(),
            7
        );
        assert_eq!(days(json!({})).unwrap(), 30);
        assert!(
            days(json!({ "publish": { "retention_days": 0 } })).is_err(),
            "zero days would delete every conversation at once; it does not read"
        );
    }
}
