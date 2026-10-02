// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The `RunProfile` of an agent run (`docs/agents.md` §3): everything that
//! makes a headless turn the run of one agent's live version.
//!
//! - **System message**: the spec's `orchestration` and `response`
//!   instructions, the slot view and each route's gate status. No chat rules,
//!   no person's memory, location or connectors. Rebuilt every round, so a
//!   slot the model just set shows up on the next one.
//! - **Tools**: the principal's grants that the spec lists, plus the run's
//!   synthetic tools (`set_<slot>`, `forward_request`). The synthetic ones are
//!   layered over the grant-narrowed source, never inside it: they are no
//!   grant, and exist only for this run.
//! - **Bound arguments** wrap the granted tools they apply to, so the model
//!   never sees nor sets them.
//! - **Budget**, **finish contract** (sub-agents only) and an injection policy
//!   of `Flag`: an agent's tool results are untrusted data by default.

use std::collections::BTreeMap;
use std::sync::Arc;

use aiplane_core::server::db::{DbError, Pool, agents as agents_db, system_principals as sp};
use aiplane_core::server::principal::SystemPrincipal;
use aiplane_core::server::run_chain::RunChain;
use aiplane_core::server::upstreams::{PoolAccess, PoolKind};
use serde_json::{Value, json};
use session_core::i18n::Lang;
use shared::api::ToolDef;

use super::bind::{BoundTool, ToolBinds, WithheldTool, without_bound};
use super::gate::{GateInput, GateStatus, RouteGates};
use super::output_filter::OutputFilter;
use super::router::{ForwardRequest, RouteClassifier, RouterSpec};
use super::slot_tools::SlotTools;
use super::state::{self, AgentState, StateSchema, render_view};
use crate::budget::Budget;
use crate::finish::FinishContract;
use crate::rama_server::state::RamaState;
use crate::server::headless::DriveParams;
use crate::server::tools::injection::{InjectionPolicy, InjectionScan};
use crate::server::tools::{Tool, ToolSource};
use aiplane_core::server::db::usage::UsageSource;

/// Why an agent could not be run.
#[derive(Debug, thiserror::Error)]
pub enum AgentRunError {
    #[error(
        "there is no enabled agent `{0}`: it was deleted, or its principal is disabled. Re-enable \
         it or route to another agent"
    )]
    Unavailable(String),
    #[error("agent `{0}` has never been published, so it has no live version to run; publish it")]
    NotLive(String),
    #[error(
        "agent `{agent}` has no version {version}, which this conversation runs on; start a new \
         conversation to use the live version"
    )]
    MissingVersion { agent: String, version: i64 },
    #[error(
        "agent `{agent}` cannot run its live version {version}: {message}. Publish a corrected \
         version"
    )]
    BadSpec {
        agent: String,
        version: i64,
        message: String,
    },
    #[error(
        "agent `{agent}` runs on pool `{pool}`, but no healthy backend of that pool serves a \
         model it may use; check the pool's backends and the agent's pool grant"
    )]
    NoModel { agent: String, pool: String },
    #[error("agent `{agent}` has no conversation `{session}`; start a new one instead")]
    UnknownSession { agent: String, session: String },
    #[error(
        "conversation `{session}` is waiting for a decision on turn `{turn}`; answer it through \
         that turn's resume route (or let it expire) before sending the next message"
    )]
    DecisionPending { session: String, turn: String },
    #[error("reading or writing the agent run failed: {0}")]
    Db(#[from] DbError),
}

/// Seams a run can be given: the clock gates and slot writes read, and the
/// route classifier. Both default to the real thing.
#[derive(Clone)]
pub struct RunOptions {
    pub now: state::Clock,
    /// `None` classifies on the agent's pool (`router.pool`, else
    /// `main.pool`). A seam because the classifier is the one model call the
    /// router makes on its own: a caller can swap it for a test double, or a
    /// deterministic stand-in, without faking an upstream.
    pub classifier: Option<Arc<dyn RouteClassifier>>,
    /// The language of text the gateway itself puts in an answer (the
    /// output filter's fallback). English until the caller knows the
    /// visitor's.
    pub lang: Lang,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            now: state::system_clock(),
            classifier: None,
            lang: Lang::En,
        }
    }
}

/// Which part an agent plays in this run.
#[derive(Debug, Clone)]
pub enum Role {
    /// The conversational agent the visitor talks to. Ends its turn with text.
    Main,
    /// Dispatched by a route: ends only through `finish`. `route_binds` are
    /// the route's `bind` values, resolved from the caller's state.
    SubAgent {
        route_binds: BTreeMap<String, Value>,
    },
}

/// One agent's live version, ready to drive.
pub struct RunProfile {
    pub principal: SystemPrincipal,
    pub version: i64,
    pub model: String,
    pub budget: Budget,
    pub finish: Option<FinishContract>,
    pub injection: InjectionScan,
    pub run: Arc<AgentRun>,
    /// `None` when the spec configures no identifier patterns.
    pub output_filter: Option<OutputFilter>,
}

/// Which spec of an agent a run loads.
#[derive(Debug, Clone)]
pub enum SpecSource {
    /// The published version the agent points at: what visitors get.
    Live,
    /// A published version, so a conversation keeps the one it started on.
    Pinned(i64),
    /// An unpublished spec, run as version [`agents_db::DRAFT_VERSION`]. Its
    /// only caller is the test-turn handler.
    Draft(Value),
}

impl RunProfile {
    /// Load agent `agent_id`'s live version and its principal's grants.
    pub async fn load(
        state: &Arc<RamaState>,
        agent_id: &str,
        role: Role,
        options: &RunOptions,
    ) -> Result<Self, AgentRunError> {
        Self::load_version(state, agent_id, None, role, options).await
    }

    /// [`Self::load`], on version `pinned` instead of the live one when it is
    /// `Some`: a conversation keeps the version it started on.
    pub async fn load_version(
        state: &Arc<RamaState>,
        agent_id: &str,
        pinned: Option<i64>,
        role: Role,
        options: &RunOptions,
    ) -> Result<Self, AgentRunError> {
        let source = pinned.map_or(SpecSource::Live, SpecSource::Pinned);
        Self::load_from(state, agent_id, source, role, options).await
    }

    /// Load agent `agent_id` from an explicit [`SpecSource`]. Only the
    /// internal test chat passes [`SpecSource::Draft`]; every other caller
    /// reaches a draft by no path at all.
    pub async fn load_from(
        state: &Arc<RamaState>,
        agent_id: &str,
        source: SpecSource,
        role: Role,
        options: &RunOptions,
    ) -> Result<Self, AgentRunError> {
        let principal = sp::load_active(&state.db, agent_id)
            .await?
            .ok_or_else(|| AgentRunError::Unavailable(agent_id.to_string()))?;
        let (version, text) = match source {
            SpecSource::Live => agents_db::live(&state.db, agent_id)
                .await?
                .ok_or_else(|| AgentRunError::NotLive(principal.name.clone()))?,
            SpecSource::Pinned(v) => agents_db::version(&state.db, agent_id, v)
                .await?
                .map(|row| (row.version, row.spec))
                .ok_or_else(|| AgentRunError::MissingVersion {
                    agent: principal.name.clone(),
                    version: v,
                })?,
            SpecSource::Draft(spec) => (agents_db::DRAFT_VERSION, spec.to_string()),
        };
        let bad = |message: String| AgentRunError::BadSpec {
            agent: principal.name.clone(),
            version,
            message,
        };
        let spec: Value =
            serde_json::from_str(&text).map_err(|e| bad(format!("it is not JSON ({e})")))?;
        let schema = Arc::new(
            StateSchema::from_spec(&spec)
                .map_err(|i| bad(format!("at `{}`, {}", i.path, i.message)))?,
        );
        let gates = Arc::new(
            RouteGates::from_spec(&spec)
                .map_err(|i| bad(format!("at `{}`, {}", i.path, i.message)))?,
        );
        let pool = spec
            .pointer("/main/pool")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("it names no `main.pool`".into()))?
            .to_string();
        let pools = PoolAccess::for_system_pools(&principal, [pool.as_str()]);
        let model = pool_model(state, &pool, &pools).ok_or_else(|| AgentRunError::NoModel {
            agent: principal.name.clone(),
            pool: pool.clone(),
        })?;
        let finish = match &role {
            Role::Main => None,
            Role::SubAgent { .. } => {
                let schema = spec.pointer("/finish/schema").cloned().ok_or_else(|| {
                    bad("it declares no `finish.schema`, which a routed sub-agent needs".into())
                })?;
                Some(FinishContract::new(schema).map_err(|e| bad(e.to_string()))?)
            }
        };

        let output_filter = OutputFilter::from_spec(&spec)
            .map_err(|e| bad(format!("`publish.output_filter`: {e}")))?;
        let mut synthetic: BTreeMap<String, Arc<dyn Tool>> = BTreeMap::new();
        let slot_tools = SlotTools::with_clock(schema.clone(), options.now.clone());
        for id in slot_tools.ids() {
            if let Some(tool) = slot_tools.get(&id) {
                synthetic.insert(id, tool);
            }
        }
        let routes = spec.get("routes").cloned().unwrap_or_else(|| json!({}));
        let conversation = (!schema.is_empty() || !gates.is_empty()).then(|| Conversation {
            schema: schema.clone(),
            gates: gates.clone(),
            routes: routes.clone(),
            now: options.now.clone(),
        });
        if !gates.is_empty() {
            let forward = ForwardRequest::new(
                state.clone(),
                Arc::new(RouterSpec {
                    principal: principal.clone(),
                    schema: schema.clone(),
                    gates,
                    routes,
                    router: spec.get("router").cloned(),
                    main_pool: pool,
                }),
                options.clone(),
            );
            synthetic.insert(forward.id().to_string(), Arc::new(forward));
        }
        let binds = match role {
            Role::Main => ToolBinds::from_spec(&spec),
            Role::SubAgent { route_binds } => ToolBinds::from_spec(&spec).with_route(route_binds),
        };
        let run = AgentRun {
            name: principal.name.clone(),
            instructions: instructions(&spec),
            conversation,
            tools: spec
                .pointer("/main/tools")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            synthetic,
            binds,
            schema: (!schema.is_empty()).then_some(schema),
            pools,
        };
        Ok(Self {
            budget: budget(&spec),
            principal,
            version,
            model,
            finish,
            injection: InjectionScan::new(InjectionPolicy::Flag),
            run: Arc::new(run),
            output_filter,
        })
    }

    /// The headless parameters that run this profile's turn in `session_id`.
    pub fn drive_params(
        &self,
        session_id: &str,
        assistant_turn_id: &str,
        chain: Arc<RunChain>,
    ) -> DriveParams {
        DriveParams {
            principal: aiplane_core::server::principal::Principal::System(self.principal.clone()),
            run: Some(chain),
            session_id: session_id.to_string(),
            assistant_turn_id: assistant_turn_id.to_string(),
            model: self.model.clone(),
            source: UsageSource::Scheduled,
            history_limit: None,
            finish: self.finish.clone(),
            budget: Some(self.budget),
            injection: self.injection.clone(),
            agent: Some(self.run.clone()),
        }
    }
}

fn budget(spec: &Value) -> Budget {
    let get = |key: &str| {
        spec.pointer(&format!("/main/budget/{key}"))
            .and_then(Value::as_u64)
    };
    Budget::new(
        get("rounds")
            .and_then(|r| u32::try_from(r).ok())
            .unwrap_or_else(|| aiplane_core::server::reasoning::Effort::Standard.max_rounds()),
        get("seconds"),
        get("tokens"),
    )
}

fn instructions(spec: &Value) -> String {
    ["orchestration", "response"]
        .iter()
        .filter_map(|k| spec.pointer(&format!("/main/instructions/{k}")))
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// A model id a healthy backend of chat pool `pool` serves, if `access` may
/// use that pool. The spec names a pool; a request names a model.
pub fn pool_model(state: &RamaState, pool: &str, access: &PoolAccess) -> Option<String> {
    let found = state
        .upstreams
        .pools()
        .into_iter()
        .find(|p| p.name == pool && p.kind == PoolKind::Chat && access.allows(p))?;
    found
        .backends
        .iter()
        .filter(|b| b.is_available())
        .flat_map(|b| b.models_snapshot())
        .min()
}

/// The main agent's conversation pieces: what its system message reports.
struct Conversation {
    schema: Arc<StateSchema>,
    gates: Arc<RouteGates>,
    routes: Value,
    now: state::Clock,
}

/// What the driver consults on every round of an agent run.
pub struct AgentRun {
    name: String,
    instructions: String,
    conversation: Option<Conversation>,
    tools: Vec<String>,
    synthetic: BTreeMap<String, Arc<dyn Tool>>,
    binds: ToolBinds,
    schema: Option<Arc<StateSchema>>,
    pools: PoolAccess,
}

impl AgentRun {
    /// The pools this run's own model calls may use: `main.pool`, if the
    /// principal holds a grant on it. The turn's rounds and the compaction
    /// of its conversation both route through it.
    pub fn pools(&self) -> &PoolAccess {
        &self.pools
    }

    /// The tools offered this round: the spec's tools the principal is
    /// granted (`granted`), then the synthetic ones.
    pub fn state_schema(&self) -> Option<&Arc<StateSchema>> {
        self.schema.as_ref()
    }

    pub fn offered(&self, granted: &[String]) -> Vec<String> {
        self.tools
            .iter()
            .filter(|t| granted.contains(t))
            .cloned()
            .chain(self.synthetic.keys().cloned())
            .collect()
    }

    /// Whether the system message changes within a turn (it carries state).
    pub fn has_conversation_state(&self) -> bool {
        self.conversation.is_some()
    }

    /// The run's leading system message.
    pub async fn system_message(
        &self,
        db: &Pool,
        session_id: &str,
        summary: Option<&str>,
    ) -> Value {
        let mut parts = vec![format!(
            "You are the agent `{}`. Follow these instructions from its owner.",
            self.name
        )];
        if !self.instructions.is_empty() {
            parts.push(self.instructions.clone());
        }
        if let Some(c) = &self.conversation {
            let state = AgentState::load(db, &c.schema, session_id)
                .await
                .unwrap_or_else(|err| {
                    tracing::warn!(error = %err, session_id, "agent state unreadable; shown as empty");
                    AgentState::default()
                });
            let view = render_view(&state.view(&c.schema));
            if !view.is_empty() {
                parts.push(view);
            }
            let input = GateInput {
                schema: &c.schema,
                state: &state,
                now: (c.now)(),
            };
            let routes = route_summary(&c.gates, &c.routes, input);
            if !routes.is_empty() {
                parts.push(routes);
            }
        }
        if let Some(summary) = summary {
            parts.push(format!(
                "Summary of the earlier part of this conversation:\n\n{summary}"
            ));
        }
        json!({ "role": "system", "content": parts.join("\n\n---\n\n") })
    }

    /// `inner` with this run's synthetic tools over it and its bound
    /// arguments applied.
    pub fn source<'a>(&'a self, inner: &'a dyn ToolSource) -> RunToolSource<'a> {
        RunToolSource {
            inner,
            run: Some(self),
        }
    }
}

fn route_summary(gates: &RouteGates, routes: &Value, input: GateInput<'_>) -> String {
    let statuses = gates.statuses(input);
    if statuses.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "Routes. When the request is complete, call forward_request: the gateway picks an open \
         route and hands the request over. You choose neither the route nor whom it concerns.",
    );
    for (name, status) in statuses {
        let about = routes
            .pointer(&format!("/{name}/description"))
            .and_then(Value::as_str)
            .map(|d| format!(" ({d})"))
            .unwrap_or_default();
        let line = match status {
            GateStatus::Open => format!("- {name}{about}: open"),
            GateStatus::Closed { missing } => {
                let why: Vec<&str> = missing.iter().map(|u| u.message.as_str()).collect();
                format!("- {name}{about}: closed — {}", why.join("; "))
            }
        };
        out.push('\n');
        out.push_str(&line);
    }
    out
}

/// A turn's tool source with an agent run layered over it. With no run it is
/// `inner` unchanged.
pub struct RunToolSource<'a> {
    inner: &'a dyn ToolSource,
    run: Option<&'a AgentRun>,
}

impl<'a> RunToolSource<'a> {
    pub fn new(inner: &'a dyn ToolSource, run: Option<&'a AgentRun>) -> Self {
        Self { inner, run }
    }

    fn bound(&self, run: &AgentRun, tool: Arc<dyn Tool>) -> Arc<dyn Tool> {
        match run.binds.for_tool(tool.id(), &tool.schema()) {
            Ok(binds) if binds.is_empty() => tool,
            Ok(binds) => Arc::new(BoundTool::new(tool, binds, run.schema.clone())),
            Err(unbound) => Arc::new(WithheldTool::new(tool, unbound)),
        }
    }
}

impl ToolSource for RunToolSource<'_> {
    fn get(&self, id: &str) -> Option<Arc<dyn Tool>> {
        let Some(run) = self.run else {
            return self.inner.get(id);
        };
        if let Some(tool) = run.synthetic.get(id) {
            return Some(tool.clone());
        }
        self.inner.get(id).map(|t| self.bound(run, t))
    }

    fn defs_for(&self, allowed: &[String]) -> Vec<ToolDef> {
        let Some(run) = self.run else {
            return self.inner.defs_for(allowed);
        };
        allowed
            .iter()
            .filter_map(|id| match run.synthetic.get(id) {
                Some(tool) => Some(tool.schema()),
                None => self
                    .inner
                    .defs_for(std::slice::from_ref(id))
                    .into_iter()
                    .next()
                    .and_then(|def| {
                        let binds = run.binds.for_tool(id, &def).ok()?;
                        Some(without_bound(def, &binds))
                    }),
            })
            .collect()
    }

    fn ids(&self) -> Vec<String> {
        let mut ids = self.inner.ids();
        if let Some(run) = self.run {
            ids.extend(run.synthetic.keys().cloned());
        }
        ids
    }

    fn contains(&self, id: &str) -> bool {
        self.run.is_some_and(|r| r.synthetic.contains_key(id)) || self.inner.contains(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::tools::echo::Echo;
    use crate::server::tools::{ToolContext, ToolRegistry};

    /// A run whose `lookup` tool binds `message` from the route: `message`
    /// is then a subject parameter, and `company_echo` declares it unbound.
    fn run() -> AgentRun {
        let spec = json!({ "main": { "tool_resources": {
            "lookup": { "bind": { "message": "route.customer" } }
        } } });
        AgentRun {
            name: "billing".into(),
            instructions: String::new(),
            conversation: None,
            tools: vec!["company_echo".into()],
            synthetic: BTreeMap::new(),
            binds: ToolBinds::from_spec(&spec)
                .with_route(BTreeMap::from([("customer".into(), json!("K-1"))])),
            schema: None,
            pools: PoolAccess::all(),
        }
    }

    #[tokio::test]
    async fn a_tool_leaving_a_subject_parameter_unbound_is_neither_offered_nor_run() {
        let registry = ToolRegistry::new().with(Echo);
        let run = run();
        let source = run.source(&registry);
        assert!(source.defs_for(&["company_echo".into()]).is_empty());

        let db = aiplane_core::server::db::open(std::path::Path::new(":memory:"))
            .await
            .unwrap();
        let err = source
            .get("company_echo")
            .expect("still resolvable, so the call is answered")
            .run(ToolContext::for_test(db), json!({"message": "K-99999"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("`message`"), "{err}");
        assert!(err.contains("not available"), "{err}");
    }
}
