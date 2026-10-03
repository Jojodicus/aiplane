// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! From the model's proposal to what the assistant offers: every piece
//! mapped to the spec fragment it stands for and checked with
//! [`spec::validate`] on the draft as it would be after the pieces before it
//! (`docs/agents.md` → "What #117 built"). A piece that would add a problem
//! to the draft, or that names something this manager may not grant, is
//! dropped with the reason in words. Nothing here writes anything.

use std::collections::{BTreeSet, HashMap};

use aiplane_core::server::principal::{GrantKind, GrantSet};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use super::proposal::{
    AbilityProposal, HandoffProposal, IdentityProposal, ScopeProposal, SlotProposal, TestProposal,
    ToneProposal,
};
use super::{Candidates, HUMAN_TARGET, IDENTITY_METHODS, MAX_TESTS, SLOT_TYPES};
use crate::agents::eval::{self, MAX_CASE_NAME_CHARS};
use crate::agents::spec::{self, SpecContext, SpecIssue, Stage};

const MAX_CHIPS: usize = 8;
const MAX_CHIP_CHARS: usize = 40;
const MAX_IDENT_LEN: usize = 48;
const TEXT_MAX: u64 = 500;
const LONG_TEXT_MAX: u64 = 2000;

/// What the agent's draft is checked against: its grants as stored now,
/// every agent and every live spec, as on a save.
pub struct ReviewContext<'a> {
    pub agent_id: &'a str,
    pub grants: &'a GrantSet,
    pub agents: &'a HashMap<String, bool>,
    pub live_specs: &'a HashMap<String, Value>,
    pub candidates: &'a Candidates,
}

/// The offer, one entry per assistant step, and what was left out.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Suggestion {
    pub steps: Steps,
    pub dropped: Vec<Dropped>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Steps {
    pub task: Option<TaskStep>,
    pub tone: Option<ToneStep>,
    pub scope: Option<ScopeStep>,
    pub abilities: Vec<AbilityStep>,
    pub slots: Vec<SlotStep>,
    pub identity: Option<IdentityStep>,
    pub handoffs: Vec<HandoffStep>,
    pub tests: Vec<TestStep>,
}

impl Steps {
    /// The steps with something to offer, for the activity event.
    pub fn offered(&self) -> Vec<&'static str> {
        [
            ("task", self.task.is_some()),
            ("tone", self.tone.is_some()),
            ("scope", self.scope.is_some()),
            ("abilities", !self.abilities.is_empty()),
            ("slots", !self.slots.is_empty()),
            ("identity", self.identity.is_some()),
            ("handoffs", !self.handoffs.is_empty()),
            ("tests", !self.tests.is_empty()),
        ]
        .into_iter()
        .filter_map(|(step, has)| has.then_some(step))
        .collect()
    }
}

/// `main.instructions.orchestration`.
#[derive(Debug, Clone, Serialize)]
pub struct TaskStep {
    pub orchestration: String,
}

/// `main.instructions.response`; `chips` are short tone words the UI shows.
#[derive(Debug, Clone, Serialize)]
pub struct ToneStep {
    pub response: String,
    pub chips: Vec<String>,
}

/// `scope.topics`, `scope.refusal`, `scope.strict`.
#[derive(Debug, Clone, Serialize)]
pub struct ScopeStep {
    pub topics: Vec<String>,
    pub refusal: String,
    pub strict: bool,
}

/// A tool for `main.tools`, which the UI grants to the agent when applied.
#[derive(Debug, Clone, Serialize)]
pub struct AbilityStep {
    pub id: String,
    pub name: String,
    pub why: String,
}

/// `state.<name>` = `def`.
#[derive(Debug, Clone, Serialize)]
pub struct SlotStep {
    pub name: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub def: Value,
}

/// A recommendation only: the method needs a connector or a key the
/// identity step asks for, so no spec fragment comes with it.
#[derive(Debug, Clone, Serialize)]
pub struct IdentityStep {
    pub method: String,
    pub why: String,
}

/// `routes.<name>` = `route`.
#[derive(Debug, Clone, Serialize)]
pub struct HandoffStep {
    pub name: String,
    pub topic: String,
    pub target: String,
    pub target_name: String,
    pub condition: Condition,
    pub route: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct Condition {
    pub slot: String,
    pub equals: Option<Value>,
}

/// A test case in the shape `POST /api/v0/agents/{id}/tests` takes.
#[derive(Debug, Clone, Serialize)]
pub struct TestStep {
    pub name: String,
    pub kind: String,
    pub script: Value,
    pub expect: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Dropped {
    pub step: &'static str,
    /// The piece within the step (a slot's or a test's name), when there is
    /// one.
    pub item: Option<String>,
    pub reason: String,
}

/// Map `answer` (the model's JSON object) onto `base` (the draft the manager
/// is editing) and keep what holds.
pub fn review(answer: &Value, base: &Value, ctx: &ReviewContext<'_>) -> Suggestion {
    let mut r = Reviewer::new(base, ctx);
    if let Some(task) = r.field::<String>(answer, "task") {
        r.task(task);
    }
    if let Some(tone) = r.field::<ToneProposal>(answer, "tone") {
        r.tone(tone);
    }
    if let Some(scope) = r.field::<ScopeProposal>(answer, "scope") {
        r.scope(scope);
    }
    for ability in r
        .field::<Vec<AbilityProposal>>(answer, "abilities")
        .unwrap_or_default()
    {
        r.ability(ability);
    }
    for slot in r
        .field::<Vec<SlotProposal>>(answer, "slots")
        .unwrap_or_default()
    {
        r.slot(slot);
    }
    if let Some(identity) = r.field::<IdentityProposal>(answer, "identity") {
        r.identity(identity);
    }
    for handoff in r
        .field::<Vec<HandoffProposal>>(answer, "handoffs")
        .unwrap_or_default()
    {
        r.handoff(handoff);
    }
    for test in r
        .field::<Vec<TestProposal>>(answer, "tests")
        .unwrap_or_default()
    {
        r.test(test);
    }
    r.out
}

struct Reviewer<'a> {
    ctx: &'a ReviewContext<'a>,
    draft: Value,
    /// The tools the offered abilities would grant: the draft is checked as
    /// if the manager had granted them, which applying the step does.
    granted: Vec<String>,
    known: BTreeSet<(String, String)>,
    tests: BTreeSet<String>,
    out: Suggestion,
}

impl<'a> Reviewer<'a> {
    fn new(base: &Value, ctx: &'a ReviewContext<'a>) -> Self {
        let draft = if base.is_object() {
            base.clone()
        } else {
            json!({})
        };
        let mut r = Self {
            ctx,
            draft,
            granted: Vec::new(),
            known: BTreeSet::new(),
            tests: BTreeSet::new(),
            out: Suggestion::default(),
        };
        r.known = r.issues(&r.draft);
        r
    }

    fn drop(&mut self, step: &'static str, item: Option<&str>, reason: impl Into<String>) {
        self.out.dropped.push(Dropped {
            step,
            item: item.map(str::to_string),
            reason: reason.into(),
        });
    }

    /// `answer[key]` as `T`; a step that does not read is dropped whole.
    fn field<T: DeserializeOwned>(&mut self, answer: &Value, key: &'static str) -> Option<T> {
        let value = answer.get(key)?;
        if value.is_null() {
            return None;
        }
        match serde_json::from_value(value.clone()) {
            Ok(v) => Some(v),
            Err(e) => {
                self.drop(
                    key,
                    None,
                    format!("the model's proposal for this step does not read: {e}"),
                );
                None
            }
        }
    }

    fn issues(&self, draft: &Value) -> BTreeSet<(String, String)> {
        let grants = GrantSet::new(
            self.ctx
                .grants
                .iter()
                .map(|(k, r)| (k, r.to_string()))
                .chain(self.granted.iter().map(|t| (GrantKind::Tool, t.clone()))),
        );
        let ctx = SpecContext {
            agent_id: self.ctx.agent_id,
            grants: &grants,
            agents: self.ctx.agents,
            live_specs: self.ctx.live_specs,
            voice_defaults: &Default::default(),
        };
        spec::validate(draft, &ctx, Stage::Draft)
            .into_iter()
            .map(|SpecIssue { path, message }| (path, message))
            .collect()
    }

    /// Keep `candidate` as the draft when it adds no problem; otherwise say
    /// which problems it would add.
    fn adopt(&mut self, candidate: Value) -> Result<(), String> {
        let found = self.issues(&candidate);
        let added: Vec<String> = found
            .difference(&self.known)
            .map(|(path, message)| {
                if path.is_empty() {
                    message.clone()
                } else {
                    format!("at `{path}`: {message}")
                }
            })
            .collect();
        if added.is_empty() {
            self.draft = candidate;
            self.known = found;
            Ok(())
        } else {
            Err(format!(
                "it would make the draft invalid — {}",
                added.join("; ")
            ))
        }
    }

    fn with(&self, pointer: &[&str], value: Value) -> Value {
        let mut draft = self.draft.clone();
        set_at(&mut draft, pointer, value);
        draft
    }

    fn task(&mut self, task: String) {
        let task = task.trim().to_string();
        if task.is_empty() {
            return self.drop("task", None, "the model proposed no task");
        }
        let candidate = self.with(&["main", "instructions", "orchestration"], json!(task));
        match self.adopt(candidate) {
            Ok(()) => {
                self.out.steps.task = Some(TaskStep {
                    orchestration: task,
                })
            }
            Err(reason) => self.drop("task", None, reason),
        }
    }

    fn tone(&mut self, tone: ToneProposal) {
        let response = tone.response.trim().to_string();
        if response.is_empty() {
            return self.drop("tone", None, "the model proposed no response instructions");
        }
        let chips: Vec<String> = tone
            .chips
            .iter()
            .map(|c| c.trim())
            .filter(|c| !c.is_empty() && c.chars().count() <= MAX_CHIP_CHARS)
            .take(MAX_CHIPS)
            .map(str::to_string)
            .collect();
        let candidate = self.with(&["main", "instructions", "response"], json!(response));
        match self.adopt(candidate) {
            Ok(()) => self.out.steps.tone = Some(ToneStep { response, chips }),
            Err(reason) => self.drop("tone", None, reason),
        }
    }

    fn scope(&mut self, scope: ScopeProposal) {
        let topics: Vec<String> = scope
            .topics
            .iter()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect();
        let refusal = scope.refusal.trim().to_string();
        let mut section = self
            .draft
            .get("scope")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        section.insert("topics".into(), json!(topics));
        section.insert("refusal".into(), json!(refusal));
        section.insert("strict".into(), json!(scope.strict));
        let candidate = self.with(&["scope"], Value::Object(section));
        match self.adopt(candidate) {
            Ok(()) => {
                self.out.steps.scope = Some(ScopeStep {
                    topics,
                    refusal,
                    strict: scope.strict,
                })
            }
            Err(reason) => self.drop("scope", None, reason),
        }
    }

    fn ability(&mut self, ability: AbilityProposal) {
        let id = ability.id.trim();
        let Some(known) = self.ctx.candidates.abilities.iter().find(|a| a.id == id) else {
            return self.drop(
                "abilities",
                Some(id),
                "it is not an ability you may grant — only tools you hold yourself can be given \
                 to an agent",
            );
        };
        if self.out.steps.abilities.iter().any(|a| a.id == id) {
            return;
        }
        let mut tools: Vec<Value> = self
            .draft
            .pointer("/main/tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !tools.iter().any(|t| t.as_str() == Some(id)) {
            tools.push(json!(id));
        }
        let candidate = self.with(&["main", "tools"], Value::Array(tools));
        self.granted.push(id.to_string());
        match self.adopt(candidate) {
            Ok(()) => self.out.steps.abilities.push(AbilityStep {
                id: id.to_string(),
                name: known.name.clone(),
                why: ability.why.trim().to_string(),
            }),
            Err(reason) => {
                self.granted.pop();
                self.drop("abilities", Some(id), reason)
            }
        }
    }

    fn slot(&mut self, slot: SlotProposal) {
        let Some(name) = ident(&slot.name) else {
            return self.drop(
                "slots",
                Some(&slot.name),
                "its name has no letters to make a slot name from",
            );
        };
        if self.draft.pointer(&format!("/state/{name}")).is_some() {
            return self.drop(
                "slots",
                Some(&name),
                format!("the draft already collects `{name}`; the existing definition stays"),
            );
        }
        let Some(mut def) = slot_def(&slot.kind, &slot.choices) else {
            return self.drop(
                "slots",
                Some(&name),
                format!(
                    "`{}` is not a kind of information the assistant knows — use one of: {}",
                    slot.kind,
                    SLOT_TYPES.join(", ")
                ),
            );
        };
        let label = slot.label.trim().to_string();
        if !label.is_empty() {
            def["description"] = json!(label);
        }
        let candidate = self.with(&["state", &name], def.clone());
        match self.adopt(candidate) {
            Ok(()) => self.out.steps.slots.push(SlotStep {
                name,
                label,
                kind: slot.kind,
                def,
            }),
            Err(reason) => self.drop("slots", Some(&name), reason),
        }
    }

    fn identity(&mut self, identity: IdentityProposal) {
        let method = identity.method.trim();
        if !IDENTITY_METHODS.contains(&method) {
            return self.drop(
                "identity",
                Some(method),
                format!(
                    "`{method}` is not an identity check — use one of: {}",
                    IDENTITY_METHODS.join(", ")
                ),
            );
        }
        self.out.steps.identity = Some(IdentityStep {
            method: method.to_string(),
            why: identity.why.trim().to_string(),
        });
    }

    fn handoff(&mut self, handoff: HandoffProposal) {
        let Some(name) = ident(&handoff.name) else {
            return self.drop(
                "handoffs",
                Some(&handoff.name),
                "its name has no letters to make a route name from",
            );
        };
        if self.draft.pointer(&format!("/routes/{name}")).is_some() {
            return self.drop(
                "handoffs",
                Some(&name),
                format!("the draft already has a hand-off `{name}`; it stays as it is"),
            );
        }
        let target = handoff.target.trim();
        let topic = handoff.topic.trim().to_string();
        let (target_name, mut route) = if target == HUMAN_TARGET {
            ("a person".to_string(), json!({ "human": {} }))
        } else {
            let Some(agent) = self
                .ctx
                .candidates
                .agents
                .iter()
                .find(|a| a.id == target && a.id != self.ctx.agent_id)
            else {
                return self.drop(
                    "handoffs",
                    Some(&name),
                    format!(
                        "`{target}` is not an agent you may hand off to — pick one of the agents \
                         shared with you, or a person"
                    ),
                );
            };
            let task = match handoff.task.trim() {
                "" => topic.clone(),
                t => t.to_string(),
            };
            (
                agent.name.clone(),
                json!({ "agent": agent.id, "task": task }),
            )
        };
        let slot = ident(&handoff.slot).unwrap_or_default();
        let equals = handoff
            .equals
            .as_deref()
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .map(|e| self.typed_value(&slot, e));
        route["when"] = match &equals {
            Some(v) => json!({ "slot": slot, "eq": v }),
            None => json!({ "slot": slot, "set": true }),
        };
        if !topic.is_empty() {
            route["description"] = json!(topic);
        }
        let candidate = self.with(&["routes", &name], route.clone());
        match self.adopt(candidate) {
            Ok(()) => self.out.steps.handoffs.push(HandoffStep {
                name,
                topic,
                target: target.to_string(),
                target_name,
                condition: Condition { slot, equals },
                route,
            }),
            Err(reason) => self.drop("handoffs", Some(&name), reason),
        }
    }

    /// `text` as the slot's type would hold it, so `eq` type-checks.
    fn typed_value(&self, slot: &str, text: &str) -> Value {
        let kind = self
            .draft
            .pointer(&format!("/state/{slot}/type"))
            .and_then(Value::as_str);
        match kind {
            Some("boolean") => text
                .parse::<bool>()
                .map_or_else(|_| json!(text), Value::Bool),
            Some("integer") => text
                .parse::<i64>()
                .map_or_else(|_| json!(text), |n| json!(n)),
            Some("number") => text
                .parse::<f64>()
                .map_or_else(|_| json!(text), |n| json!(n)),
            _ => json!(text),
        }
    }

    fn test(&mut self, test: TestProposal) {
        let name = test.name.trim().to_string();
        if self.out.steps.tests.len() >= MAX_TESTS {
            return self.drop(
                "tests",
                Some(&name),
                format!("the assistant offers at most {MAX_TESTS} test cases"),
            );
        }
        if name.is_empty() || name.chars().count() > MAX_CASE_NAME_CHARS {
            return self.drop(
                "tests",
                Some(&name),
                format!("a test case needs a name of 1 to {MAX_CASE_NAME_CHARS} characters"),
            );
        }
        if !self.tests.insert(name.clone()) {
            return self.drop(
                "tests",
                Some(&name),
                "another proposed test case has this name",
            );
        }
        let mut contains: Vec<String> = clean(&test.answer_contains);
        let not_contains = clean(&test.answer_not_contains);
        let out_of_scope = match test.kind.as_str() {
            "out_of_scope" => true,
            "in_scope" => false,
            other => {
                return self.drop(
                    "tests",
                    Some(&name),
                    format!(
                        "`{other}` is not a kind of test case — use `in_scope` or `out_of_scope`"
                    ),
                );
            }
        };
        if out_of_scope {
            let Some(refusal) = self
                .draft
                .pointer("/scope/refusal")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|r| !r.is_empty())
            else {
                return self.drop(
                    "tests",
                    Some(&name),
                    "an out-of-scope case expects the refusal, and the draft has none — accept \
                     the scope step first",
                );
            };
            contains = vec![refusal.to_string()];
        }
        let script: Vec<Value> = test
            .messages
            .iter()
            .map(|m| m.trim())
            .filter(|m| !m.is_empty())
            .map(|m| json!({ "say": m }))
            .collect();
        let mut expect = Map::new();
        expect.insert("finished".into(), json!(true));
        let mut answer = Map::new();
        if !contains.is_empty() {
            answer.insert("contains".into(), json!(contains));
        }
        if !not_contains.is_empty() {
            answer.insert("not_contains".into(), json!(not_contains));
        }
        if !answer.is_empty() {
            expect.insert("answer".into(), Value::Object(answer));
        }
        let script = Value::Array(script);
        let expect = Value::Object(expect);
        match eval::parse_case(&script, &expect) {
            Ok(_) => self.out.steps.tests.push(TestStep {
                name,
                kind: test.kind,
                script,
                expect,
            }),
            Err(issues) => {
                let reason = issues
                    .iter()
                    .map(|i| format!("at `{}`: {}", i.path, i.message))
                    .collect::<Vec<_>>()
                    .join("; ");
                self.drop(
                    "tests",
                    Some(&name),
                    format!("it is not a valid test case — {reason}"),
                )
            }
        }
    }
}

fn clean(texts: &[String]) -> Vec<String> {
    texts
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// `value` at `pointer` in `doc`, creating the objects on the way (and
/// replacing whatever non-object stood there).
fn set_at(doc: &mut Value, pointer: &[&str], value: Value) {
    let Some((last, parents)) = pointer.split_last() else {
        *doc = value;
        return;
    };
    let mut at = doc;
    for key in parents {
        if !at.is_object() {
            *at = json!({});
        }
        at = at
            .as_object_mut()
            .expect("made an object above")
            .entry(key.to_string())
            .or_insert_with(|| json!({}));
    }
    if !at.is_object() {
        *at = json!({});
    }
    at.as_object_mut()
        .expect("made an object above")
        .insert(last.to_string(), value);
}

/// `text` as a spec name: lowercase letters, digits and `_`, starting with
/// a letter.
fn ident(text: &str) -> Option<String> {
    let mut out = String::new();
    for c in text.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
    }
    let out: String = out
        .trim_start_matches(|c: char| !c.is_ascii_lowercase())
        .trim_end_matches('_')
        .chars()
        .take(MAX_IDENT_LEN)
        .collect();
    let out = out.trim_end_matches('_').to_string();
    (!out.is_empty()).then_some(out)
}

/// The slot a friendly type stands for; the model may set it.
fn slot_def(kind: &str, choices: &[String]) -> Option<Value> {
    let def = match kind {
        "text" => json!({ "type": "string", "max_length": TEXT_MAX }),
        "long_text" => json!({ "type": "string", "max_length": LONG_TEXT_MAX }),
        "email" => json!({ "type": "email" }),
        "number" => json!({ "type": "number" }),
        "whole_number" => json!({ "type": "integer" }),
        "yes_no" => json!({ "type": "boolean" }),
        "choice" => json!({ "type": "enum", "values": clean(choices) }),
        _ => return None,
    };
    let mut def = def;
    def["set_by"] = json!(["llm"]);
    Some(def)
}

#[cfg(test)]
mod tests;
