// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The agent output filter (#89): an answer may only mention identifiers
//! (customer, invoice, ticket numbers) that the run itself established.
//!
//! The spec names the identifier shapes (`publish.output_filter.patterns`).
//! Before a main agent's answer is delivered, every match of every pattern
//! must also occur in the *trusted text* of the turn: the conversation's
//! verified slots, or what this turn's tools and sub-agents returned. A match
//! found nowhere else came from the model's imagination, the visitor's own
//! message, or another customer's data it should never have seen.
//!
//! The decision is pure ([`OutputFilter::check`]); [`guard_answer`] gathers
//! the trusted text and writes the audit row.

use std::collections::BTreeSet;

use aiplane_core::server::db::agent_audit::{self, AuditKind};
use aiplane_core::server::run_chain::RunChain;
use regex::Regex;
use serde_json::{Value, json};
use session_core::db as chat;
use session_core::i18n::{Lang, t};

use super::state::{AgentState, Provenance, StateSchema};
use crate::rama_server::state::RamaState;

/// What to do with an answer that names an untraceable identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Action {
    /// Deliver nothing of it; the visitor gets a fixed fallback message. The
    /// default: a partially redacted sentence can still be misleading.
    #[default]
    Withhold,
    /// Replace each offending identifier and deliver the rest.
    Redact,
}

impl Action {
    pub const NAMES: &'static [&'static str] = &["withhold", "redact"];

    fn parse(s: &str) -> Option<Self> {
        match s {
            "withhold" => Some(Self::Withhold),
            "redact" => Some(Self::Redact),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OutputFilter {
    patterns: Vec<(String, Regex)>,
    action: Action,
}

/// The filter's decision on one answer. `hits` lists the pattern name of each
/// offending occurrence, never the matched value: the value is the thing that
/// must not leak, and the audit row is read by people with other scopes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Redacted { text: String, hits: Vec<String> },
    Withheld { hits: Vec<String> },
}

impl OutputFilter {
    /// `None` when the spec configures no patterns: the filter is off.
    pub fn from_spec(spec: &Value) -> Result<Option<Self>, String> {
        let Some(filter) = spec.pointer("/publish/output_filter") else {
            return Ok(None);
        };
        let Some(patterns) = filter
            .get("patterns")
            .and_then(Value::as_object)
            .filter(|p| !p.is_empty())
        else {
            return Ok(None);
        };
        let patterns = patterns
            .iter()
            .map(|(name, pattern)| {
                let pattern = pattern
                    .as_str()
                    .ok_or_else(|| format!("pattern `{name}` is not a string"))?;
                Regex::new(pattern)
                    .map(|re| (name.clone(), re))
                    .map_err(|e| format!("pattern `{name}` is not a valid regex: {e}"))
            })
            .collect::<Result<_, _>>()?;
        let action = match filter.get("action").and_then(Value::as_str) {
            None => Action::default(),
            Some(s) => Action::parse(s).ok_or_else(|| format!("unknown action `{s}`"))?,
        };
        Ok(Some(Self { patterns, action }))
    }

    /// Judge `answer` against `trusted`, the text the run established.
    /// `marker` replaces a redacted identifier.
    pub fn check(&self, answer: &str, trusted: &[String], marker: &str) -> Verdict {
        let known: BTreeSet<(&str, &str)> = self
            .patterns
            .iter()
            .flat_map(|(name, re)| {
                trusted.iter().flat_map(move |text| {
                    re.find_iter(text).map(move |m| (name.as_str(), m.as_str()))
                })
            })
            .collect();
        let mut offending: Vec<(usize, usize, &str)> = self
            .patterns
            .iter()
            .flat_map(|(name, re)| {
                re.find_iter(answer)
                    .filter(|m| !known.contains(&(name.as_str(), m.as_str())))
                    .map(move |m| (m.start(), m.end(), name.as_str()))
            })
            .collect();
        if offending.is_empty() {
            return Verdict::Pass;
        }
        offending.sort_unstable();
        let hits = offending.iter().map(|(_, _, n)| (*n).to_string()).collect();
        match self.action {
            Action::Withhold => Verdict::Withheld { hits },
            Action::Redact => {
                let mut text = String::with_capacity(answer.len());
                let mut at = 0;
                for (start, end, _) in offending {
                    if start < at {
                        // Two patterns matched overlapping text; the first
                        // replacement already covers it.
                        at = at.max(end);
                        continue;
                    }
                    text.push_str(&answer[at..start]);
                    text.push_str(marker);
                    at = end;
                }
                text.push_str(&answer[at..]);
                Verdict::Redacted { text, hits }
            }
        }
    }
}

/// Every string and number leaf of `value`, as text.
pub fn leaves(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Number(n) => out.push(n.to_string()),
        Value::Array(items) => items.iter().for_each(|v| leaves(v, out)),
        Value::Object(map) => map.values().for_each(|v| leaves(v, out)),
        Value::Null | Value::Bool(_) => {}
    }
}

/// The text the filter may trust for this turn. Slots the model wrote itself
/// are left out — it can copy a visitor's claim into one — as are the
/// `set_<slot>` tool calls that echo them.
async fn trusted_text(
    state: &RamaState,
    schema: Option<&StateSchema>,
    session_id: &str,
    turn_id: &str,
) -> Result<Vec<String>, aiplane_core::server::db::DbError> {
    let mut trusted = Vec::new();
    if let Some(schema) = schema {
        let slots = AgentState::load(&state.db, schema, session_id).await?;
        for def in schema.slots() {
            if let Some(entry) = slots.valid(&def.name)
                && entry.provenance != Provenance::Llm
            {
                leaves(&entry.value, &mut trusted);
            }
        }
    }
    let slot_tools: BTreeSet<String> = schema
        .into_iter()
        .flat_map(StateSchema::slots)
        .map(|d| format!("set_{}", d.name))
        .collect();
    if let Some(turn) = chat::get_turn_with_tools(&state.db, session_id, turn_id).await? {
        for call in turn
            .tool_calls
            .iter()
            .filter(|c| !slot_tools.contains(&c.name))
        {
            let Some(output) = &call.output_json else {
                continue;
            };
            match serde_json::from_str::<Value>(output) {
                Ok(v) => leaves(&v, &mut trusted),
                Err(_) => trusted.push(output.clone()),
            }
        }
    }
    Ok(trusted)
}

/// Where a filtered answer sits: the conversation, its turn and the run.
pub struct Delivery<'a> {
    pub principal_id: &'a str,
    pub session_id: &'a str,
    pub turn_id: &'a str,
    pub chain: &'a RunChain,
    pub lang: Lang,
}

/// The one call site in `run_turn`: the answer the visitor may be given.
///
/// A stored answer that fails the filter is overwritten too, so replaying the
/// conversation can never show what the delivery withheld. A failure to read
/// the trusted text withholds the answer: the filter fails closed.
pub async fn guard_answer(
    state: &RamaState,
    filter: &OutputFilter,
    schema: Option<&StateSchema>,
    at: Delivery<'_>,
    answer: String,
) -> String {
    let trusted = trusted_text(state, schema, at.session_id, at.turn_id).await;
    let verdict = match &trusted {
        Ok(trusted) => filter.check(&answer, trusted, &t(at.lang, "agent-output-redacted")),
        Err(_) => Verdict::Withheld { hits: Vec::new() },
    };
    let (delivered, hits, action) = match verdict {
        Verdict::Pass => return answer,
        Verdict::Redacted { text, hits } => (text, hits, "redacted"),
        Verdict::Withheld { hits } => (t(at.lang, "agent-output-withheld"), hits, "withheld"),
    };
    let mut detail = json!({
        "action": action,
        "session_id": at.session_id,
        "turn_id": at.turn_id,
        "patterns": hits,
    });
    if let Err(e) = &trusted {
        detail["error"] = json!(format!("the trusted text could not be read: {e}"));
    }
    if let Err(e) = agent_audit::record_run_event(
        &state.db,
        AuditKind::OutputBlocked,
        at.principal_id,
        Some(at.chain),
        detail,
    )
    .await
    {
        tracing::warn!(error = %e, "could not write the output-filter audit row");
    }
    if let Err(e) = chat::set_content(&state.db, at.turn_id, &delivered).await {
        tracing::warn!(error = %e, "could not replace the stored answer after filtering");
    }
    delivered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(action: &str) -> OutputFilter {
        OutputFilter::from_spec(&json!({"publish": {"output_filter": {
            "patterns": { "invoice": "RE-\\d{6}", "customer": "K-\\d{5}" },
            "action": action
        }}}))
        .unwrap()
        .unwrap()
    }

    fn trusted(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_patterns_means_no_filter() {
        for spec in [
            json!({}),
            json!({"publish": {}}),
            json!({"publish": {"output_filter": {}}}),
            json!({"publish": {"output_filter": {"patterns": {}}}}),
        ] {
            assert!(OutputFilter::from_spec(&spec).unwrap().is_none(), "{spec}");
        }
    }

    #[test]
    fn the_action_defaults_to_withholding() {
        let f = OutputFilter::from_spec(&json!({"publish": {"output_filter": {
            "patterns": { "invoice": "RE-\\d{6}" }
        }}}))
        .unwrap()
        .unwrap();
        assert_eq!(f.action, Action::Withhold);
    }

    #[test]
    fn a_bad_regex_or_action_is_an_error_not_a_silent_pass() {
        let bad_re = json!({"publish": {"output_filter": {"patterns": {"x": "["}}}});
        assert!(OutputFilter::from_spec(&bad_re).is_err());
        let bad_action = json!({"publish": {"output_filter": {
            "patterns": {"x": "a"}, "action": "shrug"}}});
        assert!(OutputFilter::from_spec(&bad_action).is_err());
    }

    #[test]
    fn an_answer_without_identifiers_passes() {
        assert_eq!(
            filter("withhold").check("All done, thanks.", &[], "[x]"),
            Verdict::Pass
        );
    }

    #[test]
    fn an_identifier_in_the_trusted_text_passes() {
        let v = filter("withhold").check(
            "Invoice RE-123456 for K-12345 is paid.",
            &trusted(&["K-12345", "{\"invoice\": \"RE-123456\"}"]),
            "[x]",
        );
        assert_eq!(v, Verdict::Pass);
    }

    #[test]
    fn another_customers_invoice_withholds_the_answer() {
        let v = filter("withhold").check(
            "RE-123456 is yours, and RE-999999 is also due.",
            &trusted(&["RE-123456"]),
            "[x]",
        );
        assert_eq!(
            v,
            Verdict::Withheld {
                hits: vec!["invoice".into()]
            }
        );
    }

    #[test]
    fn redaction_replaces_only_the_untraceable_identifiers() {
        let v = filter("redact").check(
            "RE-123456 is yours, RE-999999 is not; ask K-77777.",
            &trusted(&["RE-123456"]),
            "[hidden]",
        );
        assert_eq!(
            v,
            Verdict::Redacted {
                text: "RE-123456 is yours, [hidden] is not; ask [hidden].".into(),
                hits: vec!["invoice".into(), "customer".into()]
            }
        );
    }

    #[test]
    fn a_trusted_value_only_vouches_for_the_pattern_it_matched() {
        let f = OutputFilter::from_spec(&json!({"publish": {"output_filter": {
            "patterns": { "a": "X-\\d+", "b": "Y-\\d+" }, "action": "withhold"
        }}}))
        .unwrap()
        .unwrap();
        assert_eq!(f.check("X-1", &trusted(&["X-1"]), "m"), Verdict::Pass);
        assert_eq!(
            f.check("X-2", &trusted(&["Y-2", "X-1"]), "m"),
            Verdict::Withheld {
                hits: vec!["a".into()]
            }
        );
    }

    #[test]
    fn leaves_flatten_strings_and_numbers() {
        let mut out = Vec::new();
        leaves(
            &json!({"a": ["x", 4], "b": {"c": "y", "d": true}}),
            &mut out,
        );
        out.sort();
        assert_eq!(out, ["4", "x", "y"]);
    }
}
