// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The retention sweeper (`docs/agents.md` §5, "What #92 built"): deletes
//! each agent's conversations once they have been idle longer than its live
//! version's `publish.retention_days`, with their sub-agent runs, state and
//! visitor sessions. A person's chat is never touched
//! (`db::agent_retention`).

use std::collections::HashMap;
use std::time::Duration;

use aiplane_core::server::db::agent_audit::AuditKind;
use aiplane_core::server::db::agent_retention::{self, Swept};
use aiplane_core::server::db::{DbError, Pool, agents as agents_db};
use jiff::{SignedDuration, Timestamp};
use serde_json::json;

use super::spec::AgentSpec;

/// How long an agent keeps a conversation after its last message when its
/// live version sets no `publish.retention_days`, or it has none.
pub const DEFAULT_RETENTION_DAYS: i64 = 30;

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
        let days = live
            .get(&id)
            .and_then(|s| serde_json::from_str::<AgentSpec>(s).ok())
            .map_or(DEFAULT_RETENTION_DAYS, |s| s.publish.retention_days());
        let idle_before = now
            .checked_sub(SignedDuration::from_hours(days.saturating_mul(24)))
            .unwrap_or(Timestamp::MIN);
        let gone = agent_retention::delete_idle_conversations(pool, &id, idle_before).await?;
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
    use serde_json::Value;

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
