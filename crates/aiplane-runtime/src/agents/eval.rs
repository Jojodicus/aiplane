// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Agent evaluation (#99, `docs/agents.md` §5, "What #99 built"): a stored
//! test case is run as an isolated test conversation and judged on more than
//! the final answer, as a Goal-Plan-Action report.
//!
//! - **Goal**: did the conversation end as meant: finished, the answer says
//!   what it should and none of what it must not, the output filter ruled as
//!   expected.
//! - **Plan**: did the gates hold and the router choose the right route.
//! - **Action**: were the right sub-agents and tools called, with the right
//!   bound values.
//!
//! Every expectation is deterministic, read from the stored state and the
//! turn's audit rows. An optional rubric is judged by a model and reported
//! beside them in [`CaseReport::rubric`]; it never decides `passed`.
//!
//! A case runs through [`run_draft_turn`], the internal test chat's path, so
//! it is recorded as a version-0 conversation that analytics leave out and
//! retention sweeps like any other. A version is tested by passing its stored
//! spec as the draft: the same `SpecSource::Draft` door, no second path.
//!
//! The script's trusted writes use [`write_trusted`] with a [`TrustedWriter`]
//! built here from the script text. That is the one place text becomes a
//! trusted writer, and only the test runner reaches it: no public path
//! accepts a script.

use std::collections::BTreeMap;
use std::sync::Arc;

use aiplane_core::server::db::agent_audit;
use aiplane_core::server::db::agent_tests::TestCase;
use async_trait::async_trait;
use jiff::Timestamp;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use session_core::db::TurnStatus;

use super::profile::RunOptions;
use super::run::AgentTurn;
use super::run::draft::{DRAFT_VERSION, DraftDebug, collect_debug, run_draft_turn};
use super::spec_cache::CompiledSpec;
use super::state::{AgentState, StateSchema, TrustedWriter, write_trusted};
use crate::rama_server::state::RamaState;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvalIssue {
    pub path: String,
    pub message: String,
}

fn issue(path: impl Into<String>, message: impl Into<String>) -> EvalIssue {
    EvalIssue {
        path: path.into(),
        message: message.into(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Say(String),
    Write {
        slot: String,
        value: Value,
        writer: TrustedWriter,
    },
}

/// A conversation script: visitor messages with trusted slot writes between
/// them.
#[derive(Debug, Clone, PartialEq)]
pub struct Script {
    pub steps: Vec<Step>,
}

fn parse_writer(text: &str) -> Option<TrustedWriter> {
    match text {
        "host" => Some(TrustedWriter::Host),
        _ => text
            .strip_prefix("verifier:")
            .filter(|id| !id.is_empty())
            .map(|id| TrustedWriter::Verifier(id.to_string())),
    }
}

impl Script {
    /// `[{"say": "text"}, {"write": {"slot", "value", "writer"}}, …]`.
    /// `writer` is `host` or `verifier:<id>`. The conversation does not exist
    /// before the first message, so a script starts with a `say`.
    pub fn parse(value: &Value) -> Result<Self, Vec<EvalIssue>> {
        let Some(items) = value.as_array() else {
            return Err(vec![issue(
                "script",
                "must be a list of steps, such as [{\"say\": \"Hello\"}]",
            )]);
        };
        let mut issues = Vec::new();
        let mut steps = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let at = format!("script[{i}]");
            match Self::step(item, &at) {
                Ok(step) => steps.push(step),
                Err(mut found) => issues.append(&mut found),
            }
        }
        if items.is_empty() {
            issues.push(issue("script", "needs at least one `say` step"));
        } else if !matches!(steps.first(), Some(Step::Say(_))) && issues.is_empty() {
            issues.push(issue(
                "script[0]",
                "the first step must be a `say`: the conversation only exists once the visitor has \
                 spoken, so a trusted write has nothing to attach to before that",
            ));
        }
        if issues.is_empty() {
            Ok(Self { steps })
        } else {
            Err(issues)
        }
    }

    fn step(item: &Value, at: &str) -> Result<Step, Vec<EvalIssue>> {
        let Some(map) = item.as_object() else {
            return Err(vec![issue(at, "must be an object with `say` or `write`")]);
        };
        if let Some(extra) = map.keys().find(|k| *k != "say" && *k != "write") {
            return Err(vec![issue(
                format!("{at}.{extra}"),
                "unknown key; a step has `say` or `write`",
            )]);
        }
        match (map.get("say"), map.get("write")) {
            (Some(Value::String(text)), None) if !text.trim().is_empty() => {
                Ok(Step::Say(text.clone()))
            }
            (Some(_), None) => Err(vec![issue(
                format!("{at}.say"),
                "must be a non-empty string",
            )]),
            (None, Some(write)) => Self::write(write, &format!("{at}.write")),
            _ => Err(vec![issue(
                at,
                "a step has exactly one of `say` and `write`",
            )]),
        }
    }

    fn write(write: &Value, at: &str) -> Result<Step, Vec<EvalIssue>> {
        let Some(map) = write.as_object() else {
            return Err(vec![issue(
                at,
                "must be {\"slot\": …, \"value\": …, \"writer\": \"host\" | \"verifier:<id>\"}",
            )]);
        };
        let mut issues = Vec::new();
        if let Some(extra) = map
            .keys()
            .find(|k| !["slot", "value", "writer"].contains(&k.as_str()))
        {
            issues.push(issue(format!("{at}.{extra}"), "unknown key"));
        }
        let slot = map
            .get("slot")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        if slot.is_none() {
            issues.push(issue(format!("{at}.slot"), "must name a slot"));
        }
        if !map.contains_key("value") {
            issues.push(issue(format!("{at}.value"), "is required"));
        }
        let writer = map
            .get("writer")
            .and_then(Value::as_str)
            .and_then(parse_writer);
        if writer.is_none() {
            issues.push(issue(
                format!("{at}.writer"),
                "must be `host` or `verifier:<id>`; the model's own `llm` writes happen in the \
                 conversation, not in a script",
            ));
        }
        match (slot, writer, issues.is_empty()) {
            (Some(slot), Some(writer), true) => Ok(Step::Write {
                slot: slot.to_string(),
                value: map["value"].clone(),
                writer,
            }),
            _ => Err(issues),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterOutcome {
    Passed,
    Withheld,
    Redacted,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateExpect {
    pub open: bool,
    /// Slots the closed gate must still be missing; others may be too.
    #[serde(default)]
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallExpect {
    #[serde(default)]
    pub called: Vec<String>,
    #[serde(default)]
    pub not_called: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundExpect {
    pub route: String,
    pub name: String,
    pub equals: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnswerExpect {
    #[serde(default)]
    pub contains: Vec<String>,
    #[serde(default)]
    pub not_contains: Vec<String>,
}

fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

/// What a case expects, all of it deterministic. Omitted parts are not
/// checked.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    /// The gate of each named route after the last step.
    #[serde(default)]
    pub gates: BTreeMap<String, GateExpect>,
    /// The route `forward_request` picked: a name, or `null` for none.
    #[serde(default, deserialize_with = "present")]
    pub route: Option<Option<String>>,
    /// Sub-agents dispatched, by route name.
    #[serde(default)]
    pub sub_agents: CallExpect,
    /// Values a dispatched route passes to its sub-agent.
    #[serde(default)]
    pub bound: Vec<BoundExpect>,
    #[serde(default)]
    pub tools: CallExpect,
    #[serde(default)]
    pub answer: AnswerExpect,
    #[serde(default)]
    pub filter: Option<FilterOutcome>,
    /// Whether the last turn ended with an answer (`true`) or not.
    #[serde(default)]
    pub finished: Option<bool>,
}

impl Expect {
    pub fn parse(value: &Value) -> Result<Self, Vec<EvalIssue>> {
        serde_json::from_value(value.clone())
            .map_err(|e| vec![issue("expect", format!("is not a valid expectation: {e}"))])
    }

    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Parse a case's stored script and expectations, or say what to fix. A case
/// that checks nothing is refused: it would always pass.
pub fn parse_case(script: &Value, expect: &Value) -> Result<(Script, Expect), Vec<EvalIssue>> {
    let script = Script::parse(script);
    let expect = Expect::parse(expect).and_then(|e| {
        if e.is_empty() {
            Err(vec![issue(
                "expect",
                "checks nothing; add at least one expectation, such as {\"finished\": true}",
            )])
        } else {
            Ok(e)
        }
    });
    match (script, expect) {
        (Ok(script), Ok(expect)) => Ok((script, expect)),
        (script, expect) => {
            let mut issues = script.err().unwrap_or_default();
            issues.extend(expect.err().unwrap_or_default());
            Err(issues)
        }
    }
}

/// What a conversation did, as the checks read it.
#[derive(Debug, Clone, Default)]
pub struct Observed {
    /// Per route: open, and the slots its gate still misses.
    pub gates: BTreeMap<String, (bool, Vec<String>)>,
    pub picked: Vec<String>,
    pub dispatched: Vec<String>,
    /// Per route, what its `bind` resolves to from the final state.
    pub binds: BTreeMap<String, BTreeMap<String, Value>>,
    pub tools: Vec<String>,
    pub answer: Option<String>,
    pub finished: bool,
    pub filter: Option<FilterOutcome>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Check {
    pub check: String,
    pub passed: bool,
    pub expected: Value,
    pub actual: Value,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Section {
    pub passed: bool,
    pub checks: Vec<Check>,
}

impl Section {
    fn from(checks: Vec<Check>) -> Self {
        Self {
            passed: checks.iter().all(|c| c.passed),
            checks,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Sections {
    pub goal: Section,
    pub plan: Section,
    pub action: Section,
}

fn check(
    name: impl Into<String>,
    passed: bool,
    expected: Value,
    actual: Value,
    message: String,
) -> Check {
    Check {
        check: name.into(),
        passed,
        expected,
        actual,
        message,
    }
}

fn names(list: &[String]) -> String {
    if list.is_empty() {
        "none".to_string()
    } else {
        list.iter()
            .map(|n| format!("`{n}`"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Judge `seen` against `expect`. Pure: nothing here reads a model.
pub fn evaluate(expect: &Expect, seen: &Observed) -> Sections {
    let mut goal = Vec::new();
    let mut plan = Vec::new();
    let mut action = Vec::new();

    if let Some(want) = expect.finished {
        let ok = seen.finished == want;
        goal.push(check(
            "finished",
            ok,
            json!(want),
            json!(seen.finished),
            match (ok, want) {
                (true, true) => "the last turn ended with an answer".into(),
                (true, false) => "the last turn did not end with an answer, as expected".into(),
                (false, true) => "the last turn did not end with an answer: it errored, was \
                                  cancelled or is waiting for a decision a script cannot give"
                    .into(),
                (false, false) => "the last turn ended with an answer, but the case expected it \
                                   not to"
                    .into(),
            },
        ));
    }
    let answer = seen.answer.clone().unwrap_or_default();
    let lowered = answer.to_lowercase();
    for text in &expect.answer.contains {
        let ok = lowered.contains(&text.to_lowercase());
        goal.push(check(
            "answer.contains",
            ok,
            json!(text),
            json!(answer),
            if ok {
                format!("the answer mentions `{text}`")
            } else {
                format!("the answer does not mention `{text}`")
            },
        ));
    }
    for text in &expect.answer.not_contains {
        let ok = !lowered.contains(&text.to_lowercase());
        goal.push(check(
            "answer.not_contains",
            ok,
            json!(text),
            json!(answer),
            if ok {
                format!("the answer does not mention `{text}`")
            } else {
                format!("the answer mentions `{text}`, which it must not")
            },
        ));
    }
    if let Some(want) = expect.filter {
        let got = seen.filter.unwrap_or(FilterOutcome::Passed);
        goal.push(check(
            "filter",
            got == want,
            json!(want),
            json!(got),
            format!(
                "the output filter {}, expected {}",
                outcome_word(got),
                outcome_word(want)
            ),
        ));
    }

    for (route, want) in &expect.gates {
        let Some((open, missing)) = seen.gates.get(route) else {
            plan.push(check(
                format!("gates.{route}"),
                false,
                json!(want.open),
                Value::Null,
                format!("the agent has no route `{route}`; check the name in this case's `gates`"),
            ));
            continue;
        };
        let state = |open: bool| if open { "open" } else { "closed" };
        let ok = *open == want.open;
        plan.push(check(
            format!("gates.{route}"),
            ok,
            json!(state(want.open)),
            json!(state(*open)),
            if ok {
                format!("route `{route}` is {}", state(*open))
            } else {
                format!(
                    "route `{route}` is {}, expected {}{}",
                    state(*open),
                    state(want.open),
                    if *open {
                        String::new()
                    } else {
                        format!("; it still misses {}", names(missing))
                    }
                )
            },
        ));
        if !want.open && !want.missing.is_empty() {
            let absent: Vec<String> = want
                .missing
                .iter()
                .filter(|slot| !missing.contains(slot))
                .cloned()
                .collect();
            plan.push(check(
                format!("gates.{route}.missing"),
                absent.is_empty(),
                json!(want.missing),
                json!(missing),
                if absent.is_empty() {
                    format!("route `{route}` still misses {}", names(&want.missing))
                } else {
                    format!(
                        "route `{route}` was expected to miss {} but the gate no longer asks for \
                         {}; it misses {}",
                        names(&want.missing),
                        names(&absent),
                        names(missing)
                    )
                },
            ));
        }
    }
    if let Some(want) = &expect.route {
        let got = seen.picked.last();
        let ok = got == want.as_ref();
        let show =
            |r: Option<&String>| r.map_or_else(|| "no route".to_string(), |r| format!("`{r}`"));
        plan.push(check(
            "route",
            ok,
            json!(want),
            json!(got),
            format!(
                "the router picked {}, expected {}",
                show(got),
                show(want.as_ref())
            ),
        ));
    }

    calls(
        &mut action,
        "sub_agents",
        &expect.sub_agents,
        &seen.dispatched,
        "sub-agent for route",
    );
    calls(&mut action, "tools", &expect.tools, &seen.tools, "tool");
    for want in &expect.bound {
        let dispatched = seen.dispatched.contains(&want.route);
        let got = seen.binds.get(&want.route).and_then(|b| b.get(&want.name));
        let ok = dispatched && got == Some(&want.equals);
        action.push(check(
            format!("bound.{}.{}", want.route, want.name),
            ok,
            want.equals.clone(),
            got.cloned().unwrap_or(Value::Null),
            if !dispatched {
                format!(
                    "route `{}` was never dispatched, so nothing was bound for `{}`",
                    want.route, want.name
                )
            } else if ok {
                format!("route `{}` passed `{}` as expected", want.route, want.name)
            } else {
                format!(
                    "route `{}` passes `{}` = {}, expected {}",
                    want.route,
                    want.name,
                    got.map_or_else(|| "nothing".to_string(), Value::to_string),
                    want.equals
                )
            },
        ));
    }

    Sections {
        goal: Section::from(goal),
        plan: Section::from(plan),
        action: Section::from(action),
    }
}

fn outcome_word(outcome: FilterOutcome) -> &'static str {
    match outcome {
        FilterOutcome::Passed => "passed the answer",
        FilterOutcome::Withheld => "withheld the answer",
        FilterOutcome::Redacted => "redacted the answer",
    }
}

fn calls(out: &mut Vec<Check>, name: &str, want: &CallExpect, seen: &[String], what: &str) {
    for item in &want.called {
        let ok = seen.contains(item);
        out.push(check(
            format!("{name}.called"),
            ok,
            json!(item),
            json!(seen),
            if ok {
                format!("the {what} `{item}` was called")
            } else {
                format!("the {what} `{item}` was not called; it was expected to be")
            },
        ));
    }
    for item in &want.not_called {
        let ok = !seen.contains(item);
        out.push(check(
            format!("{name}.not_called"),
            ok,
            json!(item),
            json!(seen),
            if ok {
                format!("the {what} `{item}` was not called")
            } else {
                format!("the {what} `{item}` was called; it must not be")
            },
        ));
    }
}

/// The model's verdict on a rubric. Reported, never part of `passed`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RubricVerdict {
    pub passed: bool,
    pub reason: String,
}

/// What the judge reads: the visitor's messages and the agent's answers.
/// Never slot values, tool results or anything trusted.
#[derive(Debug, Clone, Serialize)]
pub struct Exchange {
    pub visitor: String,
    pub agent: Option<String>,
}

#[async_trait]
pub trait RubricJudge: Send + Sync {
    async fn judge(&self, rubric: &str, exchanges: &[Exchange]) -> Result<RubricVerdict, String>;
}

#[derive(Debug, Clone, Serialize)]
pub struct TurnReport {
    pub message: String,
    pub status: TurnStatus,
    pub answer: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RubricReport {
    /// `passed`, `failed`, `error` or `skipped`.
    pub verdict: &'static str,
    pub reason: String,
}

/// The Goal-Plan-Action report of one case.
#[derive(Debug, Clone, Serialize)]
pub struct CaseReport {
    /// The deterministic expectations all held, and the script ran through.
    pub passed: bool,
    /// Why the script stopped before its end, when it did.
    pub error: Option<String>,
    pub goal: Section,
    pub plan: Section,
    pub action: Section,
    /// `None` when the case has no rubric.
    pub rubric: Option<RubricReport>,
    pub turns: Vec<TurnReport>,
    pub debug: Option<DraftDebug>,
}

#[derive(Debug, Clone)]
pub struct CaseOutcome {
    pub report: CaseReport,
    pub session_id: Option<String>,
}

impl CaseOutcome {
    pub fn passed(&self) -> bool {
        self.report.passed
    }

    fn broken(error: String) -> Self {
        Self {
            report: CaseReport {
                passed: false,
                error: Some(error),
                goal: Section::default(),
                plan: Section::default(),
                action: Section::default(),
                rubric: None,
                turns: Vec::new(),
                debug: None,
            },
            session_id: None,
        }
    }
}

fn describe(issues: &[EvalIssue]) -> String {
    issues
        .iter()
        .map(|i| format!("{}: {}", i.path, i.message))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Run one case against `spec` as an isolated test conversation.
pub async fn run_case(
    state: &Arc<RamaState>,
    agent_id: &str,
    spec: &Value,
    case: &TestCase,
    options: &RunOptions,
    judge: Option<&dyn RubricJudge>,
) -> CaseOutcome {
    let (script, expect) = match parse_case(&case.script, &case.expect) {
        Ok(parsed) => parsed,
        Err(issues) => {
            return CaseOutcome::broken(format!(
                "the case is not valid ({}); edit it and run again",
                describe(&issues)
            ));
        }
    };
    let started = Timestamp::now();
    let mut session: Option<String> = None;
    let mut turns: Vec<TurnReport> = Vec::new();
    let mut last: Option<(String, TurnStatus, Option<String>)> = None;
    let mut error: Option<String> = None;

    let schema = StateSchema::from_spec(spec);
    let last_index = script.steps.len() - 1;
    for (i, step) in script.steps.iter().enumerate() {
        match step {
            Step::Say(message) => {
                let reply = run_draft_turn(
                    state,
                    AgentTurn {
                        agent_id,
                        session_id: session.as_deref(),
                        message,
                        visitor_id: None,
                        lang: None,
                    },
                    spec,
                    options.clone(),
                )
                .await;
                let reply = match reply {
                    Ok(reply) => reply,
                    Err(e) => {
                        error = Some(format!("step {} could not run: {e}", i + 1));
                        break;
                    }
                };
                session = Some(reply.session_id.clone());
                turns.push(TurnReport {
                    message: message.clone(),
                    status: reply.status,
                    answer: reply.answer.clone(),
                    error: reply.error.clone(),
                });
                last = Some((reply.turn_id.clone(), reply.status, reply.answer.clone()));
                if reply.status != TurnStatus::Completed && i < last_index {
                    error = Some(format!(
                        "step {} ended as `{}` instead of answering, so the script stopped: a \
                         script cannot answer an approval or a secure input",
                        i + 1,
                        serde_json::to_value(reply.status)
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_string))
                            .unwrap_or_default()
                    ));
                    break;
                }
            }
            Step::Write {
                slot,
                value,
                writer,
            } => {
                let (Ok(schema), Some(session)) = (&schema, session.as_deref()) else {
                    error = Some(format!("step {}: the spec's `state` is not valid", i + 1));
                    break;
                };
                if let Err(e) = write_trusted(
                    &state.db,
                    schema,
                    session,
                    slot,
                    value.clone(),
                    writer.clone(),
                    (options.now)(),
                )
                .await
                {
                    error = Some(format!("step {} could not write `{slot}`: {e}", i + 1));
                    break;
                }
            }
        }
    }

    let Some(session_id) = session else {
        return CaseOutcome::broken(error.unwrap_or_else(|| "the script ran no turn".into()));
    };
    let debug = match collect_debug(state, agent_id, spec, &session_id, started, options).await {
        Ok(debug) => Some(debug),
        Err(e) => {
            error.get_or_insert(format!("reading what the run left behind failed: {e}"));
            None
        }
    };
    let seen = observe(
        state,
        agent_id,
        spec,
        &session_id,
        last.as_ref(),
        debug.as_ref(),
    )
    .await;
    let sections = evaluate(&expect, &seen);
    let rubric = match (&case.rubric, judge) {
        (None, _) => None,
        (Some(_), None) => Some(RubricReport {
            verdict: "skipped",
            reason: "no judge model is available for this agent".into(),
        }),
        (Some(text), Some(judge)) => {
            let exchanges: Vec<Exchange> = turns
                .iter()
                .map(|t| Exchange {
                    visitor: t.message.clone(),
                    agent: t.answer.clone(),
                })
                .collect();
            Some(match judge.judge(text, &exchanges).await {
                Ok(v) => RubricReport {
                    verdict: if v.passed { "passed" } else { "failed" },
                    reason: v.reason,
                },
                Err(e) => RubricReport {
                    verdict: "error",
                    reason: e,
                },
            })
        }
    };
    let passed =
        error.is_none() && sections.goal.passed && sections.plan.passed && sections.action.passed;
    CaseOutcome {
        report: CaseReport {
            passed,
            error,
            goal: sections.goal,
            plan: sections.plan,
            action: sections.action,
            rubric,
            turns,
            debug,
        },
        session_id: Some(session_id),
    }
}

async fn observe(
    state: &RamaState,
    agent_id: &str,
    spec: &Value,
    session_id: &str,
    last: Option<&(String, TurnStatus, Option<String>)>,
    debug: Option<&DraftDebug>,
) -> Observed {
    let mut seen = Observed::default();
    if let Some((turn_id, status, answer)) = last {
        seen.finished = *status == TurnStatus::Completed;
        seen.answer = answer.clone();
        seen.filter = Some(filter_outcome(state, agent_id, session_id, turn_id).await);
    }
    let Some(debug) = debug else {
        return seen;
    };
    for route in &debug.routes {
        let missing = route
            .missing
            .iter()
            .filter_map(|u| u.slot.clone())
            .collect();
        seen.gates
            .insert(route.route.clone(), (route.open, missing));
    }
    let text =
        |detail: &Value, key: &str| detail.get(key).and_then(Value::as_str).map(str::to_string);
    seen.picked = debug
        .routing
        .iter()
        .filter_map(|d| text(d, "picked"))
        .collect();
    seen.dispatched = debug
        .sub_agents
        .iter()
        .filter_map(|d| text(d, "route"))
        .collect();
    seen.tools = debug
        .tool_calls
        .iter()
        .filter(|d| text(d, "decision").as_deref() == Some("allowed"))
        .filter_map(|d| text(d, "tool"))
        .collect();
    let compiled = CompiledSpec::compile(DRAFT_VERSION, spec.clone());
    if let Ok(parts) = compiled.parts()
        && let Ok(stored) = AgentState::load(&state.db, &parts.schema, session_id).await
    {
        for (route, def) in &parts.agent.routes {
            let resolved = def
                .bind
                .iter()
                .filter_map(|(name, source)| Some((name.clone(), source.resolve(&stored).ok()?)))
                .collect();
            seen.binds.insert(route.clone(), resolved);
        }
    }
    seen
}

/// What the output filter did to the answer of `turn_id`, from its audit row.
async fn filter_outcome(
    state: &RamaState,
    agent_id: &str,
    session_id: &str,
    turn_id: &str,
) -> FilterOutcome {
    let Ok(events) = agent_audit::for_principal(&state.db, agent_id).await else {
        return FilterOutcome::Passed;
    };
    events
        .iter()
        .filter(|e| e.kind == "output_blocked")
        .filter(|e| {
            e.detail.get("session_id").and_then(Value::as_str) == Some(session_id)
                && e.detail.get("turn_id").and_then(Value::as_str) == Some(turn_id)
        })
        .find_map(|e| match e.detail.get("action").and_then(Value::as_str) {
            Some("withheld") => Some(FilterOutcome::Withheld),
            Some("redacted") => Some(FilterOutcome::Redacted),
            _ => None,
        })
        .unwrap_or(FilterOutcome::Passed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen() -> Observed {
        Observed {
            gates: BTreeMap::from([
                ("billing".into(), (false, vec!["verified".into()])),
                ("technical".into(), (true, vec![])),
            ]),
            picked: vec!["technical".into()],
            dispatched: vec!["technical".into()],
            binds: BTreeMap::from([(
                "technical".into(),
                BTreeMap::from([("customer".into(), json!("C-1"))]),
            )]),
            tools: vec!["set_issue".into(), "forward_request".into()],
            answer: Some("Restart the Router.".into()),
            finished: true,
            filter: None,
        }
    }

    fn expect(v: Value) -> Expect {
        Expect::parse(&v).unwrap()
    }

    #[test]
    fn a_matching_case_passes_every_section() {
        let sections = evaluate(
            &expect(json!({
                "gates": { "billing": { "open": false, "missing": ["verified"] },
                           "technical": { "open": true } },
                "route": "technical",
                "sub_agents": { "called": ["technical"], "not_called": ["billing"] },
                "bound": [{ "route": "technical", "name": "customer", "equals": "C-1" }],
                "tools": { "called": ["forward_request"], "not_called": ["rag_search"] },
                "answer": { "contains": ["router"], "not_contains": ["refund"] },
                "filter": "passed",
                "finished": true
            })),
            &seen(),
        );
        assert!(sections.goal.passed && sections.plan.passed && sections.action.passed);
        assert_eq!(sections.goal.checks.len(), 4);
    }

    #[test]
    fn a_gate_that_is_open_when_it_should_be_closed_fails_the_plan_and_says_so() {
        let sections = evaluate(
            &expect(json!({ "gates": { "technical": { "open": false } } })),
            &seen(),
        );
        assert!(!sections.plan.passed);
        assert!(sections.goal.passed && sections.action.passed);
        assert!(
            sections.plan.checks[0]
                .message
                .contains("is open, expected closed")
        );
    }

    #[test]
    fn the_missing_slots_of_a_closed_gate_are_checked() {
        let sections = evaluate(
            &expect(json!({ "gates": { "billing": { "open": false, "missing": ["email"] } } })),
            &seen(),
        );
        assert!(sections.plan.checks[0].passed);
        assert!(!sections.plan.checks[1].passed);
        assert!(sections.plan.checks[1].message.contains("`email`"));
    }

    #[test]
    fn route_null_means_no_route_was_picked() {
        let none = Observed::default();
        assert!(
            evaluate(&expect(json!({ "route": null })), &none)
                .plan
                .passed
        );
        assert!(
            !evaluate(&expect(json!({ "route": null })), &seen())
                .plan
                .passed
        );
        assert!(
            !evaluate(&expect(json!({ "route": "billing" })), &seen())
                .plan
                .passed
        );
    }

    #[test]
    fn an_unknown_route_in_a_gate_expectation_fails_with_a_hint() {
        let sections = evaluate(
            &expect(json!({ "gates": { "ghost": { "open": true } } })),
            &seen(),
        );
        assert!(sections.plan.checks[0].message.contains("no route `ghost`"));
    }

    #[test]
    fn calls_and_binds_are_actions() {
        let sections = evaluate(
            &expect(json!({
                "sub_agents": { "called": ["billing"] },
                "tools": { "not_called": ["forward_request"] },
                "bound": [{ "route": "technical", "name": "customer", "equals": "C-2" }]
            })),
            &seen(),
        );
        assert_eq!(
            sections.action.checks.iter().filter(|c| !c.passed).count(),
            3
        );
        assert!(sections.goal.passed && sections.plan.passed);
    }

    #[test]
    fn a_bound_value_needs_the_route_to_have_been_dispatched() {
        let mut s = seen();
        s.dispatched.clear();
        let sections = evaluate(
            &expect(
                json!({ "bound": [{ "route": "technical", "name": "customer", "equals": "C-1" }] }),
            ),
            &s,
        );
        assert!(!sections.action.passed);
        assert!(
            sections.action.checks[0]
                .message
                .contains("never dispatched")
        );
    }

    #[test]
    fn the_filter_defaults_to_passed_and_is_compared() {
        let mut s = seen();
        s.filter = Some(FilterOutcome::Withheld);
        assert!(
            evaluate(&expect(json!({ "filter": "withheld" })), &s)
                .goal
                .passed
        );
        assert!(
            !evaluate(&expect(json!({ "filter": "passed" })), &s)
                .goal
                .passed
        );
    }

    #[test]
    fn answer_checks_ignore_case() {
        let s = seen();
        assert!(
            evaluate(
                &expect(json!({ "answer": { "contains": ["RESTART"] } })),
                &s
            )
            .goal
            .passed
        );
        assert!(
            !evaluate(
                &expect(json!({ "answer": { "not_contains": ["restart"] } })),
                &s
            )
            .goal
            .passed
        );
    }

    #[test]
    fn unfinished_is_expectable() {
        let mut s = seen();
        s.finished = false;
        assert!(
            evaluate(&expect(json!({ "finished": false })), &s)
                .goal
                .passed
        );
        assert!(
            !evaluate(&expect(json!({ "finished": true })), &s)
                .goal
                .passed
        );
    }

    #[test]
    fn expectations_refuse_unknown_keys_and_an_empty_case() {
        assert!(Expect::parse(&json!({ "answers": {} })).is_err());
        let issues = parse_case(&json!([{ "say": "hi" }]), &json!({})).unwrap_err();
        assert!(issues[0].message.contains("checks nothing"));
    }

    #[test]
    fn a_script_starts_with_a_say_and_names_a_trusted_writer() {
        let ok = Script::parse(&json!([
            { "say": "hello" },
            { "write": { "slot": "verified", "value": { "customer_id": "C-1" },
                         "writer": "verifier:otp" } },
            { "say": "next" }
        ]))
        .unwrap();
        assert_eq!(ok.steps.len(), 3);
        assert!(matches!(
            &ok.steps[1],
            Step::Write { writer: TrustedWriter::Verifier(id), .. } if id == "otp"
        ));

        let first = Script::parse(&json!([
            { "write": { "slot": "s", "value": 1, "writer": "host" } },
            { "say": "late" }
        ]))
        .unwrap_err();
        assert!(first[0].message.contains("first step must be a `say`"));
    }

    #[test]
    fn a_script_cannot_write_as_the_model_or_with_extra_keys() {
        let issues = Script::parse(&json!([
            { "say": "hi" },
            { "write": { "slot": "s", "value": 1, "writer": "llm" } },
            { "say": "x", "write": {} },
            { "shout": "x" },
            { "say": "" }
        ]))
        .unwrap_err();
        let paths: Vec<&str> = issues.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "script[1].write.writer",
                "script[2]",
                "script[3].shout",
                "script[4].say"
            ]
        );
    }
}
