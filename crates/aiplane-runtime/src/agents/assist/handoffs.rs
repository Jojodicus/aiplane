// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The setup assistant's hand-off model, on the server: the same routes,
//! gates and binds `readHandoffs` / `writeHandoffs` / `deriveBind` in
//! `web/src/lib/agent-setup.ts` read and write, so a hand-off the agent
//! architect writes shows in the setup as a sentence ("When it is about X,
//! hand over to Y"), not as "set up in the advanced editor".
//!
//! Keep the two in step: `web/src/lib/fixtures/architect-draft.json` is a
//! draft written here (`review/tests.rs` pins it) that `agent-setup.test.ts`
//! reads back as rules.

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

pub const TOPIC_SLOT: &str = "topic";
pub const REQUEST_SLOT: &str = "request";
pub const VERIFIED_SLOT: &str = "verified";
pub const IDENTITY_VERIFIER: &str = "identity";
pub const FALLBACK_ROUTE: &str = "fallback";
pub const HANDOFF_TASK: &str = "Request about {topic}: {request}";
const TOPIC_DESCRIPTION: &str = "What the request is about.";
const REQUEST_DESCRIPTION: &str = "A short summary of what the visitor needs.";
const LONG_TEXT_MAX: u64 = 2000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Agent(String),
    Human,
}

/// "When it is about `topic` (and all details are collected, and the
/// identity is confirmed), hand over to `target`."
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    /// The route it was read from; `None` for a new rule.
    pub route: Option<String>,
    pub topic: String,
    /// Only once every detail the agent collects is set: one `set` leaf per
    /// slot of the details step ([`detail_slots`]), written afresh whenever
    /// the rules are.
    pub details: bool,
    pub identity: bool,
    pub target: Target,
    pub bind: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Handoffs {
    pub rules: Vec<Rule>,
    /// "Otherwise hand over to a person."
    pub fallback: bool,
    /// Routes of any other shape, kept as they are.
    pub custom: Vec<String>,
}

fn request_set() -> Value {
    json!({ "slot": REQUEST_SLOT, "set": true })
}

fn is_managed(slot: &str) -> bool {
    [TOPIC_SLOT, REQUEST_SLOT, VERIFIED_SLOT].contains(&slot)
}

/// The slots of the setup's details step, in its order (`readSlots`): by
/// `order`, the rest by name.
pub fn detail_slots(spec: &Value) -> Vec<String> {
    let Some(state) = spec.get("state").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut slots: Vec<(u64, &String)> = state
        .iter()
        .filter(|(name, _)| !is_managed(name))
        .map(|(name, def)| {
            let order = def.get("order").and_then(Value::as_u64).unwrap_or(u64::MAX);
            (order, name)
        })
        .collect();
    slots.sort();
    slots.into_iter().map(|(_, name)| name.clone()).collect()
}

/// A gate leaf "detail `slot` is collected".
fn detail_leaf(leaf: &Value) -> bool {
    leaf.as_object().is_some_and(|o| {
        o.len() == 2
            && o.get("set") == Some(&json!(true))
            && o.get("slot")
                .and_then(Value::as_str)
                .is_some_and(|s| !is_managed(s))
    })
}

fn target_of(route: &Value) -> Option<Target> {
    let kinds = ["agent", "human", "a2a", "loop"]
        .iter()
        .filter(|k| route.get(**k).is_some())
        .count();
    if kinds != 1 {
        return None;
    }
    if let Some(id) = route.get("agent").and_then(Value::as_str) {
        return Some(Target::Agent(id.to_string()));
    }
    route
        .get("human")
        .filter(|h| h.is_object())
        .map(|_| Target::Human)
}

/// The rule `route` is, when it has exactly the shape [`write`] gives one.
fn rule_of(name: &str, route: &Value) -> Option<Rule> {
    let target = target_of(route)?;
    let when = route.get("when")?.as_object()?;
    if when.len() != 1 {
        return None;
    }
    let all = when.get("all")?.as_array()?;
    let [topic, request, rest @ ..] = all.as_slice() else {
        return None;
    };
    let (verified, details) = match rest {
        [details @ .., last] if !detail_leaf(last) => (Some(last), details),
        details => (None, details),
    };
    if !details.iter().all(detail_leaf) {
        return None;
    }
    if *request != request_set() {
        return None;
    }
    let topic_eq = topic.get("eq")?.as_str()?;
    if topic.get("slot")?.as_str()? != TOPIC_SLOT || topic.as_object()?.len() != 2 {
        return None;
    }
    if let Some(v) = verified {
        let ok = v.get("slot").and_then(Value::as_str) == Some(VERIFIED_SLOT)
            && v.get("provenance").is_some_and(Value::is_string)
            && v.as_object().is_some_and(|o| o.len() == 2);
        if !ok {
            return None;
        }
    }
    if matches!(target, Target::Agent(_))
        && route.get("task").and_then(Value::as_str) != Some(HANDOFF_TASK)
    {
        return None;
    }
    Some(Rule {
        route: Some(name.to_string()),
        topic: topic_eq.to_string(),
        details: !details.is_empty(),
        identity: verified.is_some(),
        target,
        bind: route
            .get("bind")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
    })
}

fn is_fallback(name: &str, route: &Value) -> bool {
    name == FALLBACK_ROUTE
        && route.get("when") == Some(&request_set())
        && target_of(route) == Some(Target::Human)
}

fn route_order(spec: &Value) -> Vec<String> {
    let names: Vec<String> = spec
        .get("routes")
        .and_then(Value::as_object)
        .map(|r| r.keys().cloned().collect())
        .unwrap_or_default();
    let mut order: Vec<String> = spec
        .pointer("/router/order")
        .and_then(Value::as_array)
        .map(|o| {
            o.iter()
                .filter_map(Value::as_str)
                .filter(|n| names.iter().any(|x| x == n))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let rest: Vec<String> = names.into_iter().filter(|n| !order.contains(n)).collect();
    order.extend(rest);
    order
}

pub fn read(spec: &Value) -> Handoffs {
    let mut out = Handoffs::default();
    for name in route_order(spec) {
        let route = &spec["routes"][&name];
        if let Some(rule) = rule_of(&name, route) {
            out.rules.push(rule);
        } else if is_fallback(&name, route) {
            out.fallback = true;
        } else {
            out.custom.push(name);
        }
    }
    out
}

/// Who writes the confirmed identity: what a gate's `provenance` names.
pub fn identity_writer(spec: &Value) -> Option<String> {
    let kind = spec
        .pointer(&format!("/verifiers/{IDENTITY_VERIFIER}/kind"))
        .and_then(Value::as_str)?;
    Some(if kind == "host_jwt" {
        "host".into()
    } else {
        format!("verifier:{IDENTITY_VERIFIER}")
    })
}

/// A route name from a topic; any spec identifier will do, the setup reads
/// a rule by its shape.
fn route_name(topic: &str, taken: &BTreeSet<String>) -> String {
    let mut base = String::new();
    for c in topic.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            base.push(c);
        } else if !base.is_empty() && !base.ends_with('_') {
            base.push('_');
        }
    }
    let base = base.trim_end_matches('_');
    let base: String = if base.starts_with(|c: char| c.is_ascii_lowercase()) {
        base.chars().take(40).collect()
    } else if base.is_empty() {
        "handoff".into()
    } else {
        format!("handoff_{base}").chars().take(40).collect()
    };
    let base = base.trim_end_matches('_').to_string();
    if !taken.contains(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}_{n}"))
        .find(|c| !taken.contains(c))
        .expect("an unused name")
}

/// Write `h` into `spec` the way the setup's hand-off step does.
pub fn write(spec: &mut Value, h: &Handoffs) {
    if !spec.is_object() {
        *spec = json!({});
    }
    let before = spec
        .get("routes")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let writer = identity_writer(spec);
    let details = detail_slots(spec);
    let rules: Vec<&Rule> = h
        .rules
        .iter()
        .filter(|r| !r.topic.trim().is_empty())
        .collect();
    let mut taken: BTreeSet<String> = h.custom.iter().cloned().collect();
    taken.insert(FALLBACK_ROUTE.into());
    let mut routes = Map::new();
    let mut rule_names = Vec::new();
    for rule in &rules {
        let topic = rule.topic.trim();
        let name = match &rule.route {
            Some(r) if !taken.contains(r) => r.clone(),
            _ => route_name(topic, &taken),
        };
        taken.insert(name.clone());
        rule_names.push(name.clone());
        let mut when = vec![json!({ "slot": TOPIC_SLOT, "eq": topic }), request_set()];
        if rule.details {
            when.extend(details.iter().map(|s| json!({ "slot": s, "set": true })));
        }
        if let (true, Some(w)) = (rule.identity, &writer) {
            when.push(json!({ "slot": VERIFIED_SLOT, "provenance": w }));
        }
        let mut route = json!({ "description": topic, "when": { "all": when } });
        match &rule.target {
            Target::Agent(id) => {
                route["agent"] = json!(id);
                route["task"] = json!(HANDOFF_TASK);
                if !rule.bind.is_empty() {
                    route["bind"] = Value::Object(rule.bind.clone());
                }
            }
            Target::Human => {
                let prev = rule.route.as_ref().and_then(|r| before.get(r));
                route["human"] = prev
                    .and_then(|p| p.get("human"))
                    .cloned()
                    .unwrap_or_else(|| json!({}));
            }
        }
        routes.insert(name, route);
    }
    for name in &h.custom {
        if let Some(route) = before.get(name) {
            routes.insert(name.clone(), route.clone());
        }
    }
    if h.fallback {
        let human = before
            .get(FALLBACK_ROUTE)
            .filter(|p| is_fallback(FALLBACK_ROUTE, p))
            .and_then(|p| p.get("human"))
            .cloned()
            .unwrap_or_else(|| json!({}));
        routes.insert(
            FALLBACK_ROUTE.into(),
            json!({ "when": request_set(), "human": human }),
        );
    }
    let has_routes = !routes.is_empty();
    spec["routes"] = Value::Object(routes);

    let mut topics: Vec<String> = Vec::new();
    for r in &rules {
        let t = r.topic.trim().to_string();
        if !topics.contains(&t) {
            topics.push(t);
        }
    }
    let state = spec
        .as_object_mut()
        .expect("made an object above")
        .entry("state")
        .or_insert_with(|| json!({}));
    if !state.is_object() {
        *state = json!({});
    }
    let state = state.as_object_mut().expect("made an object above");
    if topics.is_empty() {
        state.remove(TOPIC_SLOT);
    } else {
        state.insert(
            TOPIC_SLOT.into(),
            json!({ "type": "enum", "values": topics, "set_by": ["llm"],
                    "description": TOPIC_DESCRIPTION }),
        );
    }
    let custom_uses_request = h.custom.iter().any(|n| {
        before.get(n).is_some_and(|r| {
            r.to_string().contains(&format!("\"{REQUEST_SLOT}\""))
                || r.get("task")
                    .and_then(Value::as_str)
                    .is_some_and(|t| t.contains(&format!("{{{REQUEST_SLOT}")))
        })
    });
    if !rules.is_empty() || h.fallback {
        state.entry(REQUEST_SLOT).or_insert_with(|| {
            json!({ "type": "string", "max_length": LONG_TEXT_MAX, "set_by": ["llm"],
                    "description": REQUEST_DESCRIPTION })
        });
    } else if !custom_uses_request {
        state.remove(REQUEST_SLOT);
    }

    let mut order: Vec<String> = rule_names;
    order.extend(
        h.custom
            .iter()
            .filter(|n| spec["routes"].get(n.as_str()).is_some())
            .cloned(),
    );
    if h.fallback {
        order.push(FALLBACK_ROUTE.into());
    }
    let obj = spec.as_object_mut().expect("made an object above");
    if !order.is_empty() && has_routes {
        let router = obj
            .entry("router")
            .or_insert_with(|| json!({ "kind": "rules" }));
        if !router.is_object() {
            *router = json!({ "kind": "rules" });
        }
        router["order"] = json!(order);
    } else if let Some(router) = obj.get_mut("router").and_then(Value::as_object_mut) {
        router.remove("order");
        if router.len() == 1 && router.get("kind") == Some(&json!("rules")) {
            obj.remove("router");
        }
    }
}

/// The route values a specialist needs, filled from this agent's trusted
/// state: a slot of that name the model cannot write, else that field of the
/// confirmed identity. What neither can supply is the second list.
pub fn derive_bind(specialist: Option<&Value>, spec: &Value) -> (Map<String, Value>, Vec<String>) {
    let mut names = BTreeSet::new();
    if let Some(resources) = specialist
        .and_then(|s| s.pointer("/main/tool_resources"))
        .and_then(Value::as_object)
    {
        for resource in resources.values() {
            for source in resource
                .get("bind")
                .and_then(Value::as_object)
                .into_iter()
                .flat_map(|b| b.values())
            {
                if let Some(name) = source.as_str().and_then(|s| s.strip_prefix("route.")) {
                    names.insert(name.to_string());
                }
            }
        }
    }
    let mut bind = Map::new();
    let mut missing = Vec::new();
    for name in names {
        let slot = spec.pointer(&format!("/state/{name}"));
        let trusted = slot
            .and_then(|s| s.get("set_by"))
            .and_then(Value::as_array)
            .is_some_and(|w| !w.iter().any(|x| x == "llm"));
        if trusted {
            bind.insert(name.clone(), json!(format!("state.{name}")));
        } else if spec.pointer(&format!("/state/{VERIFIED_SLOT}")).is_some() {
            bind.insert(name.clone(), json!(format!("state.{VERIFIED_SLOT}.{name}")));
        } else {
            missing.push(name);
        }
    }
    (bind, missing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_rules_read_back_as_rules_and_other_routes_are_kept() {
        let mut spec = json!({
            "verifiers": { "identity": { "kind": "host_jwt" } },
            "routes": { "custom": { "when": { "slot": "x", "set": true }, "human": {} } },
        });
        let h = Handoffs {
            rules: vec![
                Rule {
                    route: None,
                    topic: "Invoices".into(),
                    details: false,
                    identity: true,
                    target: Target::Agent("billing".into()),
                    bind: Map::new(),
                },
                Rule {
                    route: None,
                    topic: "Complaints".into(),
                    details: false,
                    identity: false,
                    target: Target::Human,
                    bind: Map::new(),
                },
            ],
            fallback: true,
            custom: vec!["custom".into()],
        };
        write(&mut spec, &h);
        assert_eq!(
            spec["routes"]["invoices"]["when"]["all"][2],
            json!({ "slot": "verified", "provenance": "host" })
        );
        assert_eq!(
            spec["router"]["order"],
            json!(["invoices", "complaints", "custom", "fallback"])
        );
        assert_eq!(
            spec["state"]["topic"]["values"],
            json!(["Invoices", "Complaints"])
        );
        let back = read(&spec);
        assert_eq!(back.rules.len(), 2);
        assert!(back.fallback);
        assert_eq!(back.custom, ["custom"]);
        assert_eq!(back.rules[0].target, Target::Agent("billing".into()));
        assert!(back.rules[0].identity && !back.rules[1].identity);
    }

    #[test]
    fn a_rule_waiting_for_the_details_gates_on_every_detail_and_reads_back() {
        let mut spec = json!({
            "verifiers": { "identity": { "kind": "host_jwt" } },
            "state": {
                "company": { "type": "string", "set_by": ["llm"], "order": 0 },
                "email": { "type": "email", "set_by": ["llm"], "order": 1 },
                "verified": { "type": "object", "set_by": ["host"] },
            },
        });
        let rule = |identity| Rule {
            route: None,
            topic: "Qualified lead".into(),
            details: true,
            identity,
            target: Target::Human,
            bind: Map::new(),
        };
        let h = Handoffs {
            rules: vec![rule(false)],
            fallback: false,
            custom: vec![],
        };
        write(&mut spec, &h);
        assert_eq!(
            spec["routes"]["qualified_lead"]["when"]["all"],
            json!([
                { "slot": "topic", "eq": "Qualified lead" },
                { "slot": "request", "set": true },
                { "slot": "company", "set": true },
                { "slot": "email", "set": true },
            ])
        );
        let back = read(&spec);
        assert!(back.custom.is_empty(), "{:?}", back.custom);
        assert!(back.rules[0].details && !back.rules[0].identity);

        let mut both = spec.clone();
        write(
            &mut both,
            &Handoffs {
                rules: vec![rule(true)],
                ..h.clone()
            },
        );
        let back = read(&both);
        assert!(back.rules[0].details && back.rules[0].identity);
    }

    #[test]
    fn a_specialists_route_values_come_from_trusted_state_or_the_identity() {
        let specialist = json!({ "main": { "tool_resources": { "crm": { "bind": {
            "customer": "route.customer", "email": "route.email", "x": "state.x" } } } } });
        let spec = json!({ "state": {
            "customer": { "type": "string", "set_by": ["host"] },
            "email": { "type": "email", "set_by": ["llm"] } } });
        let (bind, missing) = derive_bind(Some(&specialist), &spec);
        assert_eq!(
            bind,
            *json!({ "customer": "state.customer" }).as_object().unwrap()
        );
        assert_eq!(missing, ["email"]);
    }
}
