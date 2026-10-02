// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Bound arguments and task templates (`docs/agents.md` §2, trust rules 2
//! and 3).
//!
//! A bound argument is filled in by the gateway: it is removed from the
//! schema the model sees, and whatever the model sends for it is overwritten.
//! Its value is a `{const: …}` from the spec, or a slot path read from verified
//! state at call time. A sub-agent has no state of its own; the route that
//! dispatched it resolves its `bind` from the main agent's state once, and
//! those values reach every tool of the sub-agent that declares the argument.
//!
//! A task template is the only thing a sub-agent learns of the conversation:
//! `{slot}` and `{slot.field}` placeholders filled from the main agent's state.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Map, Value};
use shared::api::ToolDef;

use super::state::{AgentState, StateSchema};
use crate::server::tools::{Tool, ToolContext, ToolError, ToolFuture};

/// Where one bound argument's value comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum BindSource {
    Const(Value),
    /// A slot path, `verified` or `verified.customer_id`.
    Slot(String),
}

impl BindSource {
    /// Read a spec's bind source: a slot path string or `{"const": v}`.
    pub fn parse(v: &Value) -> Option<Self> {
        match v {
            Value::String(path) => Some(Self::Slot(path.clone())),
            Value::Object(m) if m.len() == 1 => m.get("const").cloned().map(Self::Const),
            _ => None,
        }
    }

    /// `{arg: source}` of one `bind` object; entries that are not a source
    /// are skipped, since the validator has already refused them.
    pub fn parse_map(v: Option<&Value>) -> BTreeMap<String, BindSource> {
        v.and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(arg, src)| Self::parse(src).map(|s| (arg.clone(), s)))
            .collect()
    }

    /// The value from `state`, or what is missing.
    pub fn resolve(&self, state: &AgentState) -> Result<Value, String> {
        match self {
            Self::Const(v) => Ok(v.clone()),
            Self::Slot(path) => resolve_path(state, path),
        }
    }
}

/// The value at `slot.field…` among the valid slots of `state`.
pub fn resolve_path(state: &AgentState, path: &str) -> Result<Value, String> {
    let mut parts = path.split('.');
    let slot = parts.next().unwrap_or_default();
    let Some(entry) = state.valid(slot) else {
        return Err(format!("`{slot}` is not set"));
    };
    let mut value = &entry.value;
    for field in parts {
        value = value
            .get(field)
            .ok_or_else(|| format!("`{slot}` has no `{field}`"))?;
    }
    Ok(value.clone())
}

/// Fill every `{slot}` / `{slot.field}` of `template` from `state`. Every
/// placeholder that cannot be filled is reported, so the caller can say all
/// of what is missing at once.
pub fn render_task(template: &str, state: &AgentState) -> Result<String, Vec<String>> {
    let mut out = String::new();
    let mut missing = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            rest = "";
            break;
        };
        match resolve_path(state, after[..close].trim()) {
            Ok(Value::String(s)) => out.push_str(&s),
            Ok(other) => out.push_str(&other.to_string()),
            Err(why) => missing.push(why),
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    if missing.is_empty() {
        Ok(out)
    } else {
        Err(missing)
    }
}

/// Every bound argument of one run: per tool from `tool_resources`, and the
/// route's values for any tool that declares the argument.
#[derive(Debug, Clone, Default)]
pub struct ToolBinds {
    per_tool: BTreeMap<String, BTreeMap<String, BindSource>>,
    from_route: BTreeMap<String, Value>,
}

impl ToolBinds {
    /// The binds a spec's `main.tool_resources` declares.
    pub fn from_spec(spec: &Value) -> Self {
        let per_tool = spec
            .pointer("/main/tool_resources")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .map(|(tool, r)| (tool.clone(), BindSource::parse_map(r.get("bind"))))
            .filter(|(_, binds)| !binds.is_empty())
            .collect();
        Self {
            per_tool,
            from_route: BTreeMap::new(),
        }
    }

    /// Add the values the dispatching route resolved.
    pub fn with_route(mut self, values: BTreeMap<String, Value>) -> Self {
        self.from_route = values;
        self
    }

    /// The binds that apply to `tool`, whose model-facing schema is `def`. A
    /// tool's own `tool_resources` entry wins over a route value of the same
    /// name: it is the more specific statement of the two.
    pub fn for_tool(&self, tool: &str, def: &ToolDef) -> BTreeMap<String, BindSource> {
        let mut binds = self.per_tool.get(tool).cloned().unwrap_or_default();
        let declared = def.function.parameters.get("properties");
        for (arg, value) in &self.from_route {
            if declared.and_then(|p| p.get(arg)).is_some() {
                binds
                    .entry(arg.clone())
                    .or_insert_with(|| BindSource::Const(value.clone()));
            }
        }
        binds
    }

    pub fn is_empty(&self) -> bool {
        self.per_tool.is_empty() && self.from_route.is_empty()
    }
}

/// A tool with some arguments filled in by the gateway.
pub struct BoundTool {
    inner: Arc<dyn Tool>,
    binds: BTreeMap<String, BindSource>,
    /// For slot sources, read from the conversation at call time.
    schema: Option<Arc<StateSchema>>,
}

impl BoundTool {
    pub fn new(
        inner: Arc<dyn Tool>,
        binds: BTreeMap<String, BindSource>,
        schema: Option<Arc<StateSchema>>,
    ) -> Self {
        Self {
            inner,
            binds,
            schema,
        }
    }

    async fn bound_values(&self, ctx: &ToolContext) -> Result<Map<String, Value>, ToolError> {
        let needs_state = self
            .binds
            .values()
            .any(|s| matches!(s, BindSource::Slot(_)));
        let state = match (&self.schema, ctx.session_id.as_deref()) {
            (Some(schema), Some(session)) if needs_state => {
                AgentState::load(&ctx.db, schema, session)
                    .await
                    .map_err(|e| {
                        ToolError::Failed(format!(
                            "reading the conversation state to fill `{}`'s bound arguments: {e}",
                            self.inner.id()
                        ))
                    })?
            }
            _ => AgentState::default(),
        };
        let mut values = Map::new();
        for (arg, source) in &self.binds {
            let value = source.resolve(&state).map_err(|why| {
                ToolError::Failed(format!(
                    "`{}` cannot run yet: the gateway fills its `{arg}` from the conversation \
                     state, and {why}. Do not retry until that is set.",
                    self.inner.id()
                ))
            })?;
            values.insert(arg.clone(), value);
        }
        Ok(values)
    }
}

/// `def` without the bound arguments: not offered, not required.
pub fn without_bound(mut def: ToolDef, bound: &BTreeMap<String, BindSource>) -> ToolDef {
    let params = &mut def.function.parameters;
    if let Some(props) = params.get_mut("properties").and_then(Value::as_object_mut) {
        props.retain(|k, _| !bound.contains_key(k));
    }
    if let Some(req) = params.get_mut("required").and_then(Value::as_array_mut) {
        req.retain(|k| k.as_str().is_none_or(|k| !bound.contains_key(k)));
    }
    def
}

impl Tool for BoundTool {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn schema(&self) -> ToolDef {
        without_bound(self.inner.schema(), &self.binds)
    }

    fn run<'a>(&'a self, ctx: ToolContext, args: Value) -> ToolFuture<'a> {
        Box::pin(async move {
            let mut args = match args {
                Value::Object(map) => map,
                _ => Map::new(),
            };
            args.extend(self.bound_values(&ctx).await?);
            self.inner.run(ctx, Value::Object(args)).await
        })
    }

    fn max_duration(&self) -> Option<std::time::Duration> {
        self.inner.max_duration()
    }

    fn sensitive_args(&self) -> bool {
        self.inner.sensitive_args()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::state::tests::{at, schema};
    use aiplane_core::server::db::agent_state::StoredSlot;
    use serde_json::json;
    use std::sync::Mutex;

    fn state() -> AgentState {
        AgentState::from_rows(
            &schema(),
            vec![
                StoredSlot {
                    slot: "verified".into(),
                    value: json!({"customer_id": "K-12345"}),
                    provenance: "verifier:otp".into(),
                    set_at: at("2026-10-02T11:50:00Z"),
                },
                StoredSlot {
                    slot: "issue".into(),
                    value: json!("billing"),
                    provenance: "llm".into(),
                    set_at: at("2026-10-02T11:50:00Z"),
                },
            ],
        )
    }

    #[test]
    fn a_task_is_filled_from_state_and_reports_everything_missing() {
        assert_eq!(
            render_task(
                "Customer {verified.customer_id} asks about {issue}.",
                &state()
            ),
            Ok("Customer K-12345 asks about billing.".into())
        );
        assert_eq!(
            render_task("{email} / {verified.tier} / {issue}", &state()),
            Err(vec![
                "`email` is not set".to_string(),
                "`verified` has no `tier`".to_string()
            ])
        );
    }

    /// Records the arguments it was run with.
    struct Lookup(Mutex<Vec<Value>>);

    impl Tool for Lookup {
        fn id(&self) -> &str {
            "bound_fixture"
        }
        fn schema(&self) -> ToolDef {
            ToolDef::function(
                "bound_fixture",
                "Look up invoices.",
                json!({"type": "object",
                       "properties": {"customer_id": {"type": "string"}, "year": {"type": "integer"}},
                       "required": ["customer_id", "year"]}),
            )
        }
        fn run<'a>(&'a self, _ctx: ToolContext, args: Value) -> ToolFuture<'a> {
            self.0.lock().unwrap().push(args.clone());
            Box::pin(async move { Ok(args) })
        }
    }

    #[test]
    fn a_route_value_binds_only_tools_that_declare_the_argument_and_tool_binds_win() {
        let spec = json!({"main": {"tool_resources": {
            "bound_fixture": {"bind": {"year": {"const": 2026}}}
        }}});
        let binds = ToolBinds::from_spec(&spec).with_route(BTreeMap::from([
            ("customer_id".into(), json!("K-1")),
            ("year".into(), json!(1999)),
            ("tenant".into(), json!("t")),
        ]));
        let lookup = Lookup(Mutex::default());
        assert_eq!(
            binds.for_tool("bound_fixture", &lookup.schema()),
            BTreeMap::from([
                ("customer_id".into(), BindSource::Const(json!("K-1"))),
                ("year".into(), BindSource::Const(json!(2026))),
            ])
        );
    }

    #[tokio::test]
    async fn a_bound_argument_is_hidden_from_the_model_and_overrides_what_it_sends() {
        let db = aiplane_core::server::db::open(std::path::Path::new(":memory:"))
            .await
            .unwrap();
        let inner = Arc::new(Lookup(Mutex::default()));
        let bound = BoundTool::new(
            inner.clone(),
            BTreeMap::from([("customer_id".into(), BindSource::Const(json!("K-12345")))]),
            None,
        );
        let params = bound.schema().function.parameters;
        assert_eq!(params["properties"], json!({"year": {"type": "integer"}}));
        assert_eq!(params["required"], json!(["year"]));

        bound
            .run(
                ToolContext::for_test(db),
                json!({"customer_id": "K-99999", "year": 2025}),
            )
            .await
            .unwrap();
        assert_eq!(
            inner.0.lock().unwrap().as_slice(),
            [json!({"customer_id": "K-12345", "year": 2025})]
        );
    }

    #[tokio::test]
    async fn a_slot_bound_argument_is_read_at_call_time_and_refused_while_unset() {
        let session = "s1".to_string();
        let db = crate::agents::state::tests::pool_with_session(&session).await;
        let schema = Arc::new(schema());
        let inner = Arc::new(Lookup(Mutex::default()));
        let bound = BoundTool::new(
            inner.clone(),
            BTreeMap::from([(
                "customer_id".into(),
                BindSource::Slot("verified.customer_id".into()),
            )]),
            Some(schema.clone()),
        );
        let ctx = ToolContext {
            session_id: Some(session.clone()),
            ..ToolContext::for_test(db.clone())
        };
        let err = bound
            .run(ctx.clone(), json!({"year": 2026}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("`verified` is not set"), "{err}");
        assert!(inner.0.lock().unwrap().is_empty());

        crate::agents::state::write_trusted(
            &db,
            &schema,
            &session,
            "verified",
            json!({"customer_id": "K-777"}),
            crate::agents::state::TrustedWriter::Verifier("otp".into()),
            at("2026-10-02T11:50:00Z"),
        )
        .await
        .unwrap();
        bound.run(ctx, json!({"year": 2026})).await.unwrap();
        assert_eq!(
            inner.0.lock().unwrap().as_slice(),
            [json!({"customer_id": "K-777", "year": 2026})]
        );
    }
}
