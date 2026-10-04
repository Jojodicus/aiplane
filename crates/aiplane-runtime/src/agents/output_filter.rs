// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The agent output filter (`docs/agent-runs.md` → "Output filter"): an answer may only mention identifiers
//! (customer, invoice, ticket numbers) that the run itself established.
//!
//! The spec names the identifier shapes (`publish.output_filter.patterns`).
//! Before a main agent's answer is delivered, every match of every pattern
//! must also occur in the *trusted text* of the turn: the conversation's
//! verified slots, or what the successful tool calls of this turn and of its
//! sub-agent runs returned, less what the model passed into each call. A
//! match found nowhere else came from the model's imagination, the visitor's
//! own message, or another customer's data it should never have seen.
//!
//! The decision is pure ([`OutputFilter::check`]); [`guard_answer`] gathers
//! the trusted text and writes the audit row.

use std::collections::BTreeSet;

use aiplane_agents::db::agent_audit::AuditKind;
use aiplane_core::server::run_chain::RunChain;
use regex::Regex;
use serde_json::{Value, json};
use session_core::db as chat;
use session_core::i18n::{Lang, t};

use super::human::REQUEST_HUMAN;
use super::profile::AgentSurface;
use super::router::FORWARD_TOOL_NAME;
use super::spec::AgentSpec;
use super::state::{Provenance, StateSchema};
use crate::rama_server::state::RamaState;

/// What to do with an answer that names an untraceable identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
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
}

/// One text the run established, with what the model itself put into it.
///
/// A tool's output can repeat its arguments ("no invoice RE-99999 found"),
/// and the model chose those. An identifier found in `text` vouches for
/// itself only if [`Evidence::echoes`] says the arguments did not supply it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Evidence {
    pub text: String,
    /// The string and number leaves of the model's arguments to the call
    /// that produced `text`; empty for a verified slot.
    pub echoed: Vec<String>,
}

impl Evidence {
    pub fn verified(text: String) -> Self {
        Self {
            text,
            echoed: Vec::new(),
        }
    }

    /// Whether the model's arguments could have supplied `identifier`, with
    /// the formatting a tool adds or drops ignored: both sides are compared
    /// as their lowercase letters and digits only, so `999999`, `"re 999
    /// 999"` and `"RE-"` + `"999999"` all supply `RE-999999`.
    ///
    /// An identifier with digit runs of at least [`DISTINCT_DIGITS`] digits
    /// is supplied when every such run occurs in some argument: the runs are
    /// the part of it that tells one customer from another, a prefix is the
    /// pattern's. One without is supplied when its whole alphanumeric core
    /// occurs in one argument. Shorter runs are left out because they occur
    /// in almost any argument (`2026`, a page number) by chance.
    fn echoes(&self, identifier: &str) -> bool {
        let args: Vec<String> = self.echoed.iter().map(|a| alphanumeric(a)).collect();
        let supplied = |part: &str| args.iter().any(|arg| arg.contains(part));
        let runs: Vec<&str> = identifier
            .split(|c: char| !c.is_ascii_digit())
            .filter(|run| run.len() >= DISTINCT_DIGITS)
            .collect();
        if runs.is_empty() {
            let core = alphanumeric(identifier);
            !core.is_empty() && supplied(&core)
        } else {
            runs.into_iter().all(supplied)
        }
    }
}

/// The shortest digit run that counts as an identifier's own (see
/// [`Evidence::echoes`]).
const DISTINCT_DIGITS: usize = 4;

fn alphanumeric(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
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
    pub fn from_spec(spec: &AgentSpec) -> Result<Option<Self>, String> {
        let Some(filter) = spec
            .publish
            .output_filter
            .as_ref()
            .filter(|f| !f.patterns.is_empty())
        else {
            return Ok(None);
        };
        let patterns = filter
            .patterns
            .iter()
            .map(|(name, pattern)| {
                Regex::new(pattern)
                    .map(|re| (name.clone(), re))
                    .map_err(|e| format!("pattern `{name}` is not a valid regex: {e}"))
            })
            .collect::<Result<_, _>>()?;
        Ok(Some(Self {
            patterns,
            action: filter.action,
        }))
    }

    /// Judge `answer` against `trusted`, what the run established.
    /// `marker` replaces a redacted identifier.
    pub fn check(&self, answer: &str, trusted: &[Evidence], marker: &str) -> Verdict {
        let known: BTreeSet<(&str, &str)> = self
            .patterns
            .iter()
            .flat_map(|(name, re)| {
                trusted.iter().flat_map(move |ev| {
                    re.find_iter(&ev.text)
                        .filter(|m| !ev.echoes(m.as_str()))
                        .map(move |m| (name.as_str(), m.as_str()))
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

/// `raw` as JSON leaves, or as one string when it is not JSON.
fn leaves_of(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    match serde_json::from_str::<Value>(raw) {
        Ok(v) => leaves(&v, &mut out),
        Err(_) => out.push(raw.to_string()),
    }
    out
}

/// What one tool call of the run establishes.
///
/// Only a successful call counts: an error message is the tool talking about
/// its input. Its output is evidence with the model's arguments as the echo.
///
/// A `forward_request` result is the dispatched run's own words: a sub-agent
/// repeats its task as readily as a model repeats a visitor, and a remote
/// agent's answer is not ours to vouch for. What a sub-agent's tools
/// returned counts instead, gathered from its run below. A person's answer
/// to a handoff is trusted whole: staff read the request and wrote it.
fn evidence_of(call: &chat::ToolCall, slot_tools: &BTreeSet<String>) -> Vec<Evidence> {
    if call.status != chat::ToolCallStatus::Completed || slot_tools.contains(&call.name) {
        return Vec::new();
    }
    let Some(output) = &call.output_json else {
        return Vec::new();
    };
    let handoff = call.name == FORWARD_TOOL_NAME || call.name == REQUEST_HUMAN;
    let staff_answered =
        handoff && serde_json::from_str::<Value>(output).is_ok_and(|o| o["answered"] == true);
    if staff_answered {
        return leaves_of(output)
            .into_iter()
            .map(Evidence::verified)
            .collect();
    }
    if call.name == FORWARD_TOOL_NAME {
        return Vec::new();
    }
    let echoed = leaves_of(&call.arguments_json);
    leaves_of(output)
        .into_iter()
        .map(|text| Evidence {
            text,
            echoed: echoed.clone(),
        })
        .collect()
}

/// The assistant turns of the sub-agent runs `turn_id` dispatched.
async fn child_turns(
    state: &RamaState,
    turn_id: &str,
) -> Result<Vec<(String, String)>, aiplane_core::server::db::DbError> {
    Ok(sqlx::query_as(
        "SELECT t.session_id, t.id FROM chat_turns t
           JOIN chat_sessions s ON s.id = t.session_id
          WHERE s.parent_turn_id = ? AND t.role = 'assistant'",
    )
    .bind(turn_id)
    .fetch_all(&state.db)
    .await?)
}

/// What the filter may trust for this turn: the conversation's slots not
/// written by `llm` (the model can copy a visitor's claim into one), and the
/// outputs of the successful tool calls of the turn and of every sub-agent
/// run below it, each with the model's arguments to that call as its echo.
/// The `set_<slot>` calls are left out: they echo model-written values.
///
/// The stored arguments are the model's own: a bound argument is filled in
/// by the gateway afterwards, so a tool's repeat of it is no echo, and its
/// value came from a verified slot, which is trusted on its own anyway.
async fn trusted_text(
    state: &RamaState,
    run: &AgentSurface,
    session_id: &str,
    turn_id: &str,
) -> Result<Vec<Evidence>, aiplane_core::server::db::DbError> {
    let mut trusted = Vec::new();
    let schema = run.state_schema().map(|s| &**s);
    if let Some(schema) = schema {
        let slots = run
            .state_snapshot()
            .get(&state.db, schema, session_id)
            .await?;
        for def in schema.slots() {
            if let Some(entry) = slots.valid(&def.name)
                && entry.provenance != Provenance::Llm
            {
                let mut values = Vec::new();
                leaves(&entry.value, &mut values);
                trusted.extend(values.into_iter().map(Evidence::verified));
            }
        }
    }
    let slot_tools: BTreeSet<String> = schema
        .into_iter()
        .flat_map(StateSchema::slots)
        .map(|d| format!("set_{}", d.name))
        .collect();
    let mut pending = vec![(session_id.to_string(), turn_id.to_string())];
    let mut seen = BTreeSet::new();
    while let Some((session, turn)) = pending.pop() {
        if !seen.insert(turn.clone()) {
            continue;
        }
        if let Some(found) = chat::get_turn_with_tools(&state.db, &session, &turn).await? {
            for call in &found.tool_calls {
                trusted.extend(evidence_of(call, &slot_tools));
            }
        }
        pending.extend(child_turns(state, &turn).await?);
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
    run: &AgentSurface,
    at: Delivery<'_>,
    answer: String,
) -> String {
    let trusted = trusted_text(state, run, at.session_id, at.turn_id).await;
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
        "original": answer,
        "delivered": delivered,
    });
    if let Err(e) = &trusted {
        detail["error"] = json!(format!("the trusted text could not be read: {e}"));
    }
    super::audit::record(
        &state.db,
        AuditKind::OutputBlocked,
        at.principal_id,
        None,
        Some(at.chain),
        detail,
    )
    .await;
    if let Err(e) = chat::set_content(&state.db, at.turn_id, &delivered).await {
        tracing::warn!(error = %e, "could not replace the stored answer after filtering");
    }
    delivered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_spec(spec: &Value) -> Result<Option<OutputFilter>, String> {
        let spec = AgentSpec::from_value(spec).map_err(|e| e.to_string())?;
        OutputFilter::from_spec(&spec)
    }

    fn filter(action: &str) -> OutputFilter {
        from_spec(&json!({"publish": {"output_filter": {
            "patterns": { "invoice": "RE-\\d{6}", "customer": "K-\\d{5}" },
            "action": action
        }}}))
        .unwrap()
        .unwrap()
    }

    fn trusted(texts: &[&str]) -> Vec<Evidence> {
        texts
            .iter()
            .map(|s| Evidence::verified(s.to_string()))
            .collect()
    }

    fn output(text: &str, args: &[&str]) -> Evidence {
        Evidence {
            text: text.into(),
            echoed: args.iter().map(|a| a.to_string()).collect(),
        }
    }

    #[test]
    fn an_identifier_a_tool_was_given_does_not_vouch_for_itself() {
        let f = filter("withhold");
        let not_found = [output("no invoice RE-999999 found", &["RE-999999"])];
        assert_eq!(
            f.check("RE-999999 is paid.", &not_found, "[x]"),
            Verdict::Withheld {
                hits: vec!["invoice".into()]
            }
        );
        let found = [output(
            "{\"invoice\": \"RE-500000\", \"replaced_by\": \"RE-500001\"}",
            &["please look up RE-500000"],
        )];
        assert_eq!(
            f.check("Replaced by RE-500001.", &found, "[x]"),
            Verdict::Pass
        );
        assert_eq!(
            f.check("RE-500000 is replaced.", &found, "[x]"),
            Verdict::Withheld {
                hits: vec!["invoice".into()]
            }
        );
    }

    #[test]
    fn an_echo_in_one_call_leaves_another_source_of_the_same_value_standing() {
        let v = filter("withhold").check(
            "K-12345 owes RE-123456.",
            &[
                output("customer K-12345: RE-123456", &["K-12345"]),
                Evidence::verified("K-12345".into()),
            ],
            "[x]",
        );
        assert_eq!(v, Verdict::Pass);
    }

    #[test]
    fn an_echo_is_recognised_through_reformatting() {
        let f = filter("withhold");
        for args in [
            &["999999"][..],
            &["re 999 999"],
            &["Invoice 999-999"],
            &["RE-", "999999"],
        ] {
            assert_eq!(
                f.check(
                    "RE-999999 is paid.",
                    &[output("RE-999999: paid", args)],
                    "[x]"
                ),
                Verdict::Withheld {
                    hits: vec!["invoice".into()]
                },
                "{args:?}"
            );
        }
        let year_scoped = from_spec(&json!({"publish": {"output_filter": {
            "patterns": { "invoice": "RE-\\d{4}-\\d{4}" }
        }}}))
        .unwrap()
        .unwrap();
        let found = [output("RE-2026-0042", &["2026"])];
        assert_eq!(
            year_scoped.check("RE-2026-0042", &found, "[x]"),
            Verdict::Pass
        );
        let echoed = [output("RE-2026-0042", &["2026", "42"])];
        assert_eq!(
            year_scoped.check("RE-2026-0042", &echoed, "[x]"),
            Verdict::Pass,
            "a short run of the arguments is no echo of a longer one"
        );
        let both = [output("RE-2026-0042", &["2026", "0042"])];
        assert!(matches!(
            year_scoped.check("RE-2026-0042", &both, "[x]"),
            Verdict::Withheld { .. }
        ));
    }

    #[test]
    fn a_lookup_by_another_identifier_vouches_for_what_it_returned() {
        let v = filter("withhold").check(
            "RE-123456 is open.",
            &[output("{\"invoices\": [\"RE-123456\"]}", &["K-12345"])],
            "[x]",
        );
        assert_eq!(v, Verdict::Pass);
    }

    #[test]
    fn an_identifier_without_a_long_digit_run_is_echoed_by_its_alphanumeric_core() {
        let f = from_spec(&json!({"publish": {"output_filter": {
            "patterns": { "ticket": "T-[A-Z]{3}" }
        }}}))
        .unwrap()
        .unwrap();
        assert!(matches!(
            f.check("T-ABC", &[output("ticket T-ABC", &["t abc"])], "[x]"),
            Verdict::Withheld { .. }
        ));
        assert_eq!(
            f.check("T-ABC", &[output("ticket T-ABC", &["T-XYZ"])], "[x]"),
            Verdict::Pass
        );
    }

    fn tool_call(name: &str, args: Value, output: Value) -> chat::ToolCall {
        chat::ToolCall {
            id: "c".into(),
            turn_id: "t".into(),
            seq: 0,
            name: name.into(),
            arguments_json: args.to_string(),
            output_json: Some(output.to_string()),
            status: chat::ToolCallStatus::Completed,
            created_at: jiff::Timestamp::now(),
            completed_at: None,
        }
    }

    #[test]
    fn a_successful_calls_output_is_evidence_echoing_its_arguments() {
        let call = tool_call("lookup", json!({"id": "RE-1"}), json!({"next": "RE-2"}));
        assert_eq!(
            evidence_of(&call, &BTreeSet::new()),
            [output("RE-2", &["RE-1"])]
        );
        let mut errored = call;
        errored.status = chat::ToolCallStatus::Errored;
        assert_eq!(evidence_of(&errored, &BTreeSet::new()), []);
        let slots = BTreeSet::from(["set_issue".to_string()]);
        let set = tool_call("set_issue", json!({"value": "x"}), json!("x"));
        assert_eq!(evidence_of(&set, &slots), []);
    }

    #[test]
    fn a_forward_request_result_counts_only_when_a_person_answered() {
        let none = BTreeSet::new();
        let forwarded = json!({"forwarded": true, "outcome": {"answer": "RE-1"}});
        let call = tool_call(FORWARD_TOOL_NAME, json!({}), forwarded);
        assert_eq!(evidence_of(&call, &none), []);
        let staff = json!({"answered": true, "answer": "RE-1"});
        for name in [FORWARD_TOOL_NAME, REQUEST_HUMAN] {
            let call = tool_call(name, json!({"question": "RE-1?"}), staff.clone());
            assert!(
                evidence_of(&call, &none).contains(&Evidence::verified("RE-1".into())),
                "{name}"
            );
        }
    }

    #[test]
    fn no_patterns_means_no_filter() {
        for spec in [
            json!({}),
            json!({"publish": {}}),
            json!({"publish": {"output_filter": {}}}),
            json!({"publish": {"output_filter": {"patterns": {}}}}),
        ] {
            assert!(from_spec(&spec).unwrap().is_none(), "{spec}");
        }
    }

    #[test]
    fn the_action_defaults_to_withholding() {
        let f = from_spec(&json!({"publish": {"output_filter": {
            "patterns": { "invoice": "RE-\\d{6}" }
        }}}))
        .unwrap()
        .unwrap();
        assert_eq!(f.action, Action::Withhold);
    }

    #[test]
    fn a_bad_regex_or_action_is_an_error_not_a_silent_pass() {
        let bad_re = json!({"publish": {"output_filter": {"patterns": {"x": "["}}}});
        assert!(from_spec(&bad_re).is_err());
        let bad_action = json!({"publish": {"output_filter": {
            "patterns": {"x": "a"}, "action": "shrug"}}});
        assert!(from_spec(&bad_action).is_err());
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
        let f = from_spec(&json!({"publish": {"output_filter": {
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
