// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! What an agent did over a time range, counted from rows that already exist
//! (`docs/agents.md` §5, "What #100 built").
//!
//! There is no second event store: conversations and turns come from the chat
//! tables, outcomes from `agent_audit`, cost and tokens from `usage_events`.
//! Nothing a visitor typed or an agent answered is read — only counts, route
//! and slot names, and reason kinds the spec's author chose.
//!
//! Test conversations (the builder's draft chat, recorded as version
//! [`DRAFT_VERSION`]) are left out everywhere, so a manager trying an agent
//! never moves its numbers. Rows are aggregated in Rust rather than in SQL:
//! the version lives inside the serialized call chain, and timestamps are
//! RFC 3339 strings whose fractional seconds vary in length, so SQL only
//! narrows by day and the exact range is applied here.

use std::collections::BTreeMap;

use jiff::{Timestamp, ToSpan, tz::TimeZone};
use serde::Serialize;
use serde_json::Value;

use super::agents::DRAFT_VERSION;
use super::{DbError, Pool};

/// The audit kind #96 writes when a conversation is handed to a person. Named
/// here so the count starts moving the day that kind exists; until then it is 0.
pub const HUMAN_HANDOFF_KIND: &str = "human_handoff";

const AUDIT_KINDS: [&str; 6] = [
    "route_decision",
    "sub_agent_dispatched",
    "sub_agent_finished",
    "output_blocked",
    "limit_refused",
    HUMAN_HANDOFF_KIND,
];

/// A half-open range `[from, to)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub from: Timestamp,
    pub to: Timestamp,
}

impl Range {
    fn contains(&self, at: Timestamp) -> bool {
        self.from <= at && at < self.to
    }

    fn day_prefixes(&self) -> (String, String) {
        let day = |t: Timestamp| t.to_string()[..10].to_string();
        (day(self.from), day(self.to + 24.hours()))
    }
}

#[derive(Debug, Default, Serialize, PartialEq)]
pub struct SubAgents {
    pub dispatched: u64,
    pub finished: u64,
    pub incomplete: u64,
    pub incomplete_by_reason: BTreeMap<String, u64>,
}

#[derive(Debug, Default, Serialize, PartialEq)]
pub struct MissingSlot {
    pub route: String,
    pub slot: String,
    pub count: u64,
}

#[derive(Debug, Default, Serialize, PartialEq)]
pub struct GateRefusals {
    pub total: u64,
    pub by_route: BTreeMap<String, u64>,
    pub by_missing_slot: Vec<MissingSlot>,
}

#[derive(Debug, Default, Serialize, PartialEq)]
pub struct Blocks {
    pub total: u64,
    pub by_action: BTreeMap<String, u64>,
}

#[derive(Debug, Default, Serialize, PartialEq)]
pub struct LimitRefusals {
    pub total: u64,
    pub by_kind: BTreeMap<String, u64>,
}

#[derive(Debug, Default, Serialize, PartialEq)]
pub struct Spend {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub tokens: u64,
    pub cost: f64,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Day {
    pub day: String,
    pub conversations: u64,
    pub turns: u64,
    pub tokens: u64,
    pub cost: f64,
    pub refusals: u64,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Analytics {
    pub from: String,
    pub to: String,
    pub version: Option<i64>,
    pub conversations: u64,
    pub turns: u64,
    pub sub_agents: SubAgents,
    pub gate_refusals: GateRefusals,
    pub routes_chosen: BTreeMap<String, u64>,
    pub output_blocks: Blocks,
    pub limit_refusals: LimitRefusals,
    pub human_handoffs: u64,
    pub usage: Spend,
    pub daily: Vec<Day>,
}

/// Whether a row belongs in the numbers. `chain` is the serialized call chain
/// on the row; `agent` is the main agent the numbers are for.
///
/// A chain whose main agent is someone else is that agent's run, and a chain
/// at the draft version is a builder test. A row with no chain (a visitor
/// refused before any run) has no version, so it counts only when the caller
/// did not ask for one.
fn counts(chain: Option<&Value>, agent: &str, version: Option<i64>) -> bool {
    let Some(chain) = chain else {
        return version.is_none();
    };
    let main = &chain["frames"][0];
    if main["principal_id"].as_str() != Some(agent) {
        return false;
    }
    let held = main["version"].as_i64();
    if held == Some(DRAFT_VERSION) {
        return false;
    }
    version.is_none_or(|v| held == Some(v))
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

fn day_of(at: Timestamp) -> String {
    at.to_zoned(TimeZone::UTC).date().to_string()
}

fn empty_days(range: Range) -> BTreeMap<String, Day> {
    let mut days = BTreeMap::new();
    let mut at = range
        .from
        .to_zoned(TimeZone::UTC)
        .start_of_day()
        .map_or(range.from, |z| z.timestamp());
    while at < range.to {
        let day = day_of(at);
        days.insert(
            day.clone(),
            Day {
                day,
                conversations: 0,
                turns: 0,
                tokens: 0,
                cost: 0.0,
                refusals: 0,
            },
        );
        at += 24.hours();
    }
    days
}

fn bucket(days: &mut BTreeMap<String, Day>, at: Timestamp) -> &mut Day {
    days.get_mut(&day_of(at))
        .expect("a day inside the range has a bucket")
}

fn in_range(range: Range, created: &str) -> Option<Timestamp> {
    created
        .parse::<Timestamp>()
        .ok()
        .filter(|at| range.contains(*at))
}

fn parsed_chain(chain: Option<String>) -> Option<Value> {
    chain.and_then(|c| serde_json::from_str(&c).ok())
}

pub async fn compute(
    pool: &Pool,
    agent_id: &str,
    range: Range,
    version: Option<i64>,
) -> Result<Analytics, DbError> {
    let (lo, hi) = range.day_prefixes();
    let mut out = Analytics {
        from: range.from.to_string(),
        to: range.to.to_string(),
        version,
        conversations: 0,
        turns: 0,
        sub_agents: SubAgents::default(),
        gate_refusals: GateRefusals::default(),
        routes_chosen: BTreeMap::new(),
        output_blocks: Blocks::default(),
        limit_refusals: LimitRefusals::default(),
        human_handoffs: 0,
        usage: Spend::default(),
        daily: Vec::new(),
    };
    let mut days = empty_days(range);

    let sessions: Vec<(String,)> = sqlx::query_as(
        "SELECT created_at FROM chat_sessions
          WHERE principal_id = ? AND user_id IS NULL AND parent_turn_id IS NULL
            AND agent_version IS NOT ? AND (? IS NULL OR agent_version = ?)
            AND created_at >= ? AND created_at < ?",
    )
    .bind(agent_id)
    .bind(DRAFT_VERSION)
    .bind(version)
    .bind(version)
    .bind(&lo)
    .bind(&hi)
    .fetch_all(pool)
    .await?;
    for (created,) in sessions {
        if let Some(at) = in_range(range, &created) {
            out.conversations += 1;
            bucket(&mut days, at).conversations += 1;
        }
    }

    let turns: Vec<(String,)> = sqlx::query_as(
        "SELECT t.created_at FROM chat_turns t
           JOIN chat_sessions s ON s.id = t.session_id
          WHERE s.principal_id = ? AND s.user_id IS NULL AND s.parent_turn_id IS NULL
            AND s.agent_version IS NOT ? AND (? IS NULL OR s.agent_version = ?)
            AND t.role = 'user' AND t.created_at >= ? AND t.created_at < ?",
    )
    .bind(agent_id)
    .bind(DRAFT_VERSION)
    .bind(version)
    .bind(version)
    .bind(&lo)
    .bind(&hi)
    .fetch_all(pool)
    .await?;
    for (created,) in turns {
        if let Some(at) = in_range(range, &created) {
            out.turns += 1;
            bucket(&mut days, at).turns += 1;
        }
    }

    let kinds = AUDIT_KINDS.map(|k| format!("'{k}'")).join(",");
    let audit: Vec<(String, Option<String>, String, String)> = sqlx::query_as(&format!(
        "SELECT kind, chain, detail, created_at FROM agent_audit
          WHERE principal_id = ? AND kind IN ({kinds})
            AND created_at >= ? AND created_at < ?"
    ))
    .bind(agent_id)
    .bind(&lo)
    .bind(&hi)
    .fetch_all(pool)
    .await?;
    let mut missing: BTreeMap<(String, String), u64> = BTreeMap::new();
    for (kind, chain, detail, created) in audit {
        let Some(at) = in_range(range, &created) else {
            continue;
        };
        if !counts(parsed_chain(chain).as_ref(), agent_id, version) {
            continue;
        }
        let detail: Value = serde_json::from_str(&detail).unwrap_or(Value::Null);
        match kind.as_str() {
            "route_decision" => match text(&detail, "picked") {
                Some(route) => *out.routes_chosen.entry(route).or_default() += 1,
                None if detail["reason"] == "no_open_route" => {
                    out.gate_refusals.total += 1;
                    bucket(&mut days, at).refusals += 1;
                    for gate in detail["routes"].as_array().into_iter().flatten() {
                        let Some(route) = text(gate, "route") else {
                            continue;
                        };
                        if gate["gate"]["status"] != "closed" {
                            continue;
                        }
                        *out.gate_refusals.by_route.entry(route.clone()).or_default() += 1;
                        for unmet in gate["gate"]["missing"].as_array().into_iter().flatten() {
                            if let Some(slot) = text(unmet, "slot") {
                                *missing.entry((route.clone(), slot)).or_default() += 1;
                            }
                        }
                    }
                }
                None => {}
            },
            "sub_agent_dispatched" => out.sub_agents.dispatched += 1,
            "sub_agent_finished" => match detail["outcome"]["status"].as_str() {
                Some("finished") => out.sub_agents.finished += 1,
                Some("incomplete") => {
                    out.sub_agents.incomplete += 1;
                    let reason = text(&detail["outcome"]["reason"], "kind")
                        .unwrap_or_else(|| "unknown".into());
                    *out.sub_agents
                        .incomplete_by_reason
                        .entry(reason)
                        .or_default() += 1;
                }
                _ => {}
            },
            "output_blocked" => {
                out.output_blocks.total += 1;
                let action = text(&detail, "action").unwrap_or_else(|| "unknown".into());
                *out.output_blocks.by_action.entry(action).or_default() += 1;
            }
            "limit_refused" => {
                out.limit_refusals.total += 1;
                bucket(&mut days, at).refusals += 1;
                let limit = text(&detail, "limit").unwrap_or_else(|| "unknown".into());
                *out.limit_refusals.by_kind.entry(limit).or_default() += 1;
            }
            HUMAN_HANDOFF_KIND => out.human_handoffs += 1,
            _ => {}
        }
    }
    out.gate_refusals.by_missing_slot = missing
        .into_iter()
        .map(|((route, slot), count)| MissingSlot { route, slot, count })
        .collect();

    type UsageRow = (
        String,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        f64,
        Option<String>,
    );
    let usage: Vec<UsageRow> = sqlx::query_as(
        "SELECT created_at, prompt_tokens, completion_tokens, total_tokens, cost, chain
           FROM usage_events
          WHERE agent_id = ? AND created_at >= ? AND created_at < ?",
    )
    .bind(agent_id)
    .bind(&lo)
    .bind(&hi)
    .fetch_all(pool)
    .await?;
    for (created, prompt, completion, total, cost, chain) in usage {
        let Some(at) = in_range(range, &created) else {
            continue;
        };
        if !counts(parsed_chain(chain).as_ref(), agent_id, version) {
            continue;
        }
        let (prompt, completion) = (prompt.unwrap_or(0) as u64, completion.unwrap_or(0) as u64);
        let tokens = total.map_or(prompt + completion, |t| t as u64);
        out.usage.requests += 1;
        out.usage.prompt_tokens += prompt;
        out.usage.completion_tokens += completion;
        out.usage.tokens += tokens;
        out.usage.cost += cost;
        let day = bucket(&mut days, at);
        day.tokens += tokens;
        day.cost += cost;
    }
    out.daily = days.into_values().collect();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn chain(main: &str, version: Option<i64>) -> Value {
        json!({ "frames": [{ "principal_id": main, "version": version }] })
    }

    #[test]
    fn a_draft_run_never_counts() {
        assert!(!counts(Some(&chain("a", Some(0))), "a", None));
    }

    #[test]
    fn another_agents_run_never_counts() {
        assert!(!counts(Some(&chain("b", Some(1))), "a", None));
    }

    #[test]
    fn the_version_filter_keeps_only_that_version_and_drops_chainless_rows() {
        assert!(counts(Some(&chain("a", Some(2))), "a", Some(2)));
        assert!(!counts(Some(&chain("a", Some(1))), "a", Some(2)));
        assert!(!counts(None, "a", Some(2)));
        assert!(counts(None, "a", None));
    }

    #[test]
    fn a_range_asks_the_database_only_for_whole_days() {
        let range = Range {
            from: "2026-10-01T13:00:00Z".parse().unwrap(),
            to: "2026-10-03T01:00:00Z".parse().unwrap(),
        };
        assert_eq!(
            range.day_prefixes(),
            ("2026-10-01".to_string(), "2026-10-04".to_string())
        );
    }

    #[test]
    fn every_day_in_the_range_has_a_bucket_even_with_no_activity() {
        let range = Range {
            from: "2026-10-01T13:00:00Z".parse().unwrap(),
            to: "2026-10-03T01:00:00Z".parse().unwrap(),
        };
        let days: Vec<_> = empty_days(range).into_keys().collect();
        assert_eq!(days, ["2026-10-01", "2026-10-02", "2026-10-03"]);
    }
}
