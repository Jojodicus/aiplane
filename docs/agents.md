# Agents

Design for the agent builder (epic #75). People with the agent-management
permission compose an **agent**: a conversational main agent, typed state, hard
gates, a router, specialised sub-agents and human-in-the-loop. They embed it on
their own website, where it talks to visitors who are never trusted.

This is the design record the implementation issues (#77–#97) build against.
Where it says *decided*, the decision came from the epic discussion. Where it
says *chosen*, this doc picked it and the reason is next to it.

## Contents

1. [Principals](#1-principals)
2. [Agent definition](#2-agent-definition)
3. [Run model](#3-run-model)
4. [Gates and validation](#4-gates-and-validation)
5. [Visitor sessions and embedding](#5-visitor-sessions-and-embedding)
6. [Crate placement](#6-crate-placement)
7. [Issue map](#7-issue-map)

## Trust rules

Every section below enforces these five rules. Code that breaks one is a bug,
even if it works.

1. **Security lives in gateway code, not in the model.** Gates, grants, bound
   arguments and verifier results are decided in Rust. An LLM may only *deny*,
   never *grant*.
2. **The model never picks the subject.** Whose data a tool touches comes from
   verified state, bound into the call by the gateway and hidden from the schema
   the model sees.
3. **Sub-agents get a task, not the transcript.** Their input is a template
   rendered from state. Their output is a schema-checked `finish` result that
   returns to the main agent as data.
4. **No privilege escalation.** A system principal starts with nothing. A
   manager can only grant what they hold at grant time.
5. **Enforce at the tool layer.** Every tool call is checked against the acting
   principal's grants, including calls inside nested sub-agents, and audited
   with the full call chain.

## 1. Principals

### What exists today

Every credential resolves to a person:
- `tokens.user_id → users(id)` (`migrations/0001_init.sql`).
- `require_bearer` (`aiplane-runtime/src/rama_server/auth.rs`) builds a
  `UserCtx` from the user row.
- `ToolContext` carries `user_id: String` and `roles: Vec<String>`
  (`aiplane-runtime/src/server/tools/mod.rs`).

Three behaviours make that unsafe to reuse for agents:
- `Resolver::role_ids_for` puts **every `is_default` group** in front of the
  mapped ones.
- `Resolver::resource_allowed` treats an **empty `allowed_groups` as "everyone"**.
  That covers pools (and so the models they serve), RAG collections and MCP
  connectors.
- The MCP manager, memory, personal skills and per-user tool prefs are all keyed
  by `user_id`.

An agent that went through any of these paths would silently inherit
company-wide rights. That contradicts the decided default-deny.

### Decided

- Agents run as **named system principals**, never on a user's token.
- A new principal has **no rights**. Tools, connectors, skills, RAG collections
  and models are granted one by one, and nothing company-wide is inherited.
- Only holders of the **agent-management permission** create or configure
  principals.
- A manager can only grant **what they hold themselves at grant time**. After
  that a grant persists until the principal is reconfigured, independent of the
  manager's later rights. A token a non-admin manager minted, though, carries
  only what that manager holds at each request
  ([`auth.md`](auth.md#system-principals-and-gws_-tokens)).
- **Sub-agents are agents.** Each one is its own principal with its own grants.
- A **visitor is not a principal**. A visitor is a session under the agent.

### Chosen: a separate principal table, not a `kind` column on `users`

A `users` row with `kind = 'agent'` would keep every `user_id` foreign key
working for free. It would also feed agents into all three inheritance paths
above, plus the OIDC upsert by `sub`, admin user lists and impersonation. Each
of those would need a guard, and a missed guard is a silent grant. A separate
table makes the compiler find every site instead.

```sql
CREATE TABLE system_principals (
    id          TEXT PRIMARY KEY NOT NULL,   -- uuid v4
    name        TEXT NOT NULL UNIQUE,        -- slug, shown in usage/audit: "support-website"
    display     TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    created_by  TEXT NOT NULL,               -- users.id, audit only
    created_at  TEXT NOT NULL,
    disabled_at TEXT                         -- disables every token + embed key
) STRICT;

CREATE TABLE principal_grants (
    principal_id TEXT NOT NULL REFERENCES system_principals(id) ON DELETE CASCADE,
    kind         TEXT NOT NULL,              -- 'tool' | 'connector' | 'skill' | 'rag_collection' | 'model'
    ref          TEXT NOT NULL,              -- tool id, connector key, skill name, collection id, model name
    granted_by   TEXT NOT NULL,              -- users.id
    granted_at   TEXT NOT NULL,
    PRIMARY KEY (principal_id, kind, ref)
) STRICT;

CREATE TABLE system_tokens (                 -- the standalone use: CI, integrations
    id           TEXT PRIMARY KEY NOT NULL,
    principal_id TEXT NOT NULL REFERENCES system_principals(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    hash         TEXT NOT NULL UNIQUE,       -- SHA-256 of the bearer, like tokens.hash
    created_by   TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    last_used_at TEXT,
    expires_at   TEXT NOT NULL,
    revoked_at   TEXT
) STRICT;

ALTER TABLE gateway_groups ADD COLUMN can_manage_agents INTEGER NOT NULL DEFAULT 0;
```

- **The table is separate from `tokens`, with its own prefix `gws_`.** User
  tokens stay `gwk_`. `require_bearer` routes by prefix, so a user-token lookup
  can never return a principal and the other way round. All existing token code
  stays untouched.
- **`can_manage_agents`** works like `is_admin` on a gateway group: a column,
  checked by `Resolver::can_manage_agents(role_ids)`. `is_admin` implies it.
- **Grant-time cap.** Writing a grant runs the manager's own check for that
  resource: `allowed_tools`, `resource_allowed` for collections and
  connectors, `allowed_skills`, and for a model the list the person's own chat
  picker is built from (`server::model_choices::offered`, [Models](#models)).
  A refused grant returns an actionable error naming the resource. Grants are
  never re-derived later.
- **Who manages a principal.** An agent's principal: whoever holds a share on
  the agent (§2). Any other principal: its creator (`created_by`) and admins,
  nobody else — another manager gets 404. Issuing a token takes the cap for
  every grant at once: a non-admin must hold all of them right now
  (`403 token_exceeds_manager`), because the token hands them all out. See
  [`auth.md`](auth.md#management-api).
- **Every grant change is audited** in `agent_audit` (§3). The table landed with
  #77 (migration `0077_agent_builder.sql`), with one column more than §3 lists: `actor_id`, the
  user who made a management change, so "who" is queryable rather than buried in
  `detail`. `chain` is `NULL` on those rows.

### The `Principal` type

```rust
// aiplane-core: identity only, no AppState
pub enum Principal {
    User { id: String, roles: Vec<String> },
    System(SystemPrincipal),
}

pub struct SystemPrincipal {
    pub id: String,
    pub name: String,
    pub grants: Arc<GrantSet>,      // loaded once per run from principal_grants
}

impl Principal {
    /// Present only for a person. Tools that act for a person
    /// (memory, notify_user, schedule_action, location, browser_control)
    /// refuse with a clear error when this is None.
    pub fn user_id(&self) -> Option<&str>;
    /// Stable id for scoping rows and attribution: users.id or system_principals.id.
    pub fn subject_id(&self) -> &str;
    pub fn kind(&self) -> PrincipalKind;   // 'user' | 'system'
}
```

`ToolContext.user_id` and `ToolContext.roles` are replaced by
`principal: Principal`, and `ToolContext` gains `run: Option<Arc<RunChain>>`
(§3). There is no compatibility shim: the roughly 70 call sites in
`aiplane-tools` and `aiplane-runtime` move to `user_id()` or `subject_id()`.
That compile pass is the audit of every place identity matters.

### How each subsystem treats a system principal

| Subsystem | `Principal::User` | `Principal::System` |
|---|---|---|
| Tool offer (`AppState::allowed_tools_for_*`) | groups ∩ registry, per-conversation narrowing | `grants[tool]` ∩ registry, plus the run's synthetic tools (§3). No `enable_tools` bootstrap and no `"*"` |
| Skills (`allowed_skills_for`, `read_skill`) | groups plus overlay | `grants[skill]` only |
| MCP (`McpManager::layer_for_user`) | the user's connected per-user connectors plus global ones | `grants[connector]`, restricted to connectors with scope `global` or `agent`. `user_mcp` is never read |
| RAG collections | `resource_allowed` | `grants[rag_collection]`. An empty `allowed_groups` does **not** count |
| Models (`PoolAccess`) | pool `allowed_groups` plus the token allowlist | `grants[model]` only (`PoolAccess::granted_models`): each granted name routes only through the pools its grant records, as itself or as what a backend there resolves it to; a granted automatic route also reaches its candidates, fallback and selector through its pools |
| Memory, personal skills, user tool prefs | yes | no access: `user_id()` is `None` |
| Usage | `usage_events.user_id` | `usage_events.principal_kind = 'system'`, with the principal id in `user_id` |
| Limits | subject `user`/`role`/`global` | subject `system`: an operator's cap on the agent, next to the owner's `publish.budget` ([§5](#what-92-built)) |
| Audit | `mcp_tool_audit.user_id` | plus `principal_kind` and `chain` columns |

- **New connector scope `agent`.** It sits next to `per_user` and `global` in
  `mcp_catalog`. Such a connector is invisible to every person and usable only
  by principals that were granted it. This is how an owner wires their own ERP
  or ticket MCP, with a static bearer, without exposing it to employees.
- **Why `global` cannot do this.** A `global` connector with empty
  `allowed_groups` is open to everyone, so it cannot serve this purpose.
- **Usage columns.** The rows rename nothing. The pre-1.0 rule would allow
  renaming `user_id` to `principal_id`, but the column already holds "who is
  billed". Adding `principal_kind` keeps every existing query correct for users.

## 2. Agent definition

An agent is a system principal plus a spec.

```sql
CREATE TABLE agents (
    principal_id  TEXT PRIMARY KEY NOT NULL REFERENCES system_principals(id) ON DELETE CASCADE,
    draft_spec    TEXT NOT NULL,              -- JSON, freely editable
    live_version  INTEGER,                    -- NULL = never published
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
) STRICT;

CREATE TABLE agent_versions (
    principal_id  TEXT NOT NULL REFERENCES agents(principal_id) ON DELETE CASCADE,
    version       INTEGER NOT NULL,
    spec          TEXT NOT NULL,              -- immutable snapshot
    published_by  TEXT NOT NULL,
    published_at  TEXT NOT NULL,
    PRIMARY KEY (principal_id, version)
) STRICT;

CREATE TABLE agent_shares (
    principal_id  TEXT NOT NULL REFERENCES agents(principal_id) ON DELETE CASCADE,
    subject_kind  TEXT NOT NULL,              -- 'user' | 'group'
    subject_id    TEXT NOT NULL,
    access        TEXT NOT NULL,              -- 'read' | 'write'
    PRIMARY KEY (principal_id, subject_kind, subject_id)
) STRICT;
```

**Drafts and versions**
- Edits go to `draft_spec`. Publishing validates the draft, snapshots it as
  version `n+1` and moves `live_version`.
- Visitors and parent agents always run the live version. A draft runs only in
  the internal test chat.
- Rolling back means setting `live_version` to an older snapshot.

**Shares**
- The creator gets a `write` share automatically.
- Every share takes effect only for a holder of `can_manage_agents`.
- *Chosen:* `read` shows the spec and the agent's conversations, and those
  hold visitor data, so a read share needs the permission too. Support staff
  who only answer handoffs use the HiL inbox (#96), which needs no share:
  they are the agent's *responders* ([#96](#what-96-built)).

**Grants are not versioned.** They belong to the principal and persist until
reconfigured, as decided. If a live spec references a tool whose grant was
revoked, it hits default-deny at run time and `on_tool_unavailable` decides what
happens (§3).

### Spec layout

Stored as JSON. The builder UI and the API speak the same shape. YAML here is
only for readability.

```yaml
profile: { display: "croit Support", avatar: null, color: null }
scope:                                  # #115; all optional
  topics: [croit products, Ceph storage]
  refusal: "I can only help with croit products and Ceph storage."
  strict: true                          # a topic guard refuses everything else
  classifier_model: small-fast          # the guard's model; default the main model, must be granted
main:
  model: qwen3                          # must be in grants[model]; unset = the gateway's default chat model
  instructions:
    orchestration: "Collect name, email and issue before forwarding …"
    response:      "Friendly, short, in the visitor's language …"
  tools: [rag_search]                   # must be in grants[tool]; mcp__<key>__<tool> needs grants[connector]
  skills: [brand-voice]                 # must be in grants[skill]
  tool_resources:
    rag_search: { bind: { collection: { const: "produktdoku" } } }
  budget: { rounds: 12, seconds: 60, tokens: 40000 }
state:                                  # slot name -> definition
  name:     { type: string, max_length: 120, set_by: [llm] }
  email:    { type: email,  set_by: [llm] }
  issue:    { type: enum, values: [billing, technical, sales], set_by: [llm] }
  verified: { type: subject, set_by: ["verifier:otp", host] }
  issue_summary: { type: string, max_length: 2000, set_by: [llm] }
  # any slot may carry `order: <n>` (n ≥ 0): its place in the setup's list
  # of details (#116); nothing at run time reads it
verifiers:
  otp: { kind: mcp_code, connector: erp, send_tool: send_code, check_tool: check_code,
         input: secure_field, max_attempts: 5, code_ttl: 10m }
router: { kind: rules, order: [billing] } # or { kind: classifier, model: small-fast }
routes:
  billing:
    when: { all: [ { slot: issue, eq: billing },
                   { slot: verified, provenance: "verifier:otp", max_age: 15m } ] }
    agent: 5b1c…                        # the agent id (system_principals.id) of another agent
    task: "Invoice question from customer {verified.customer_id}: {issue_summary}"
    bind: { customer: state.verified.customer_id }   # passed to the sub-agent as route.customer
  technical:
    when: { slot: issue, eq: technical }
    agent: 9e04…
    task: "{issue_summary}"
  human:
    when: { slot: issue, set: true }
    human: { notify: [push, slack], inbox: support, timeout: 30m }   # #96
finish: { schema: { type: object, required: [answer], properties: {
            answer: { type: string }, facts: { type: array, items: { type: string } },
            needs_human: { type: boolean } } } }
on_tool_unavailable: reject             # reject | skip
publish:
  origins: ["https://www.example.com"]
  idle_ttl: 30m
  retention_days: 30                    # default 30
  rate_limits:                          # defaults: visitor 20 per 10m, ip 60 per 10m
    visitor: { max: 20, per: 10m }      # messages per visitor session
    ip:      { max: 60, per: 10m }      # conversations started + messages per client IP
  budget: { monthly_cost: 50, monthly_tokens: 2000000 }   # no default
  output_filter:
    patterns: { invoice: "RE-\\d{6}", customer: "K-\\d{5}" }
    action: withhold                    # withhold (default) | redact
  require_passing_tests: false          # true: publish needs a green test run of this draft (#99)
  a2a: { enabled: true }                # serve over A2A (#102); skills: [{id, name, description}] optional
```

A sub-agent's spec uses the same layout. It has no `state`, `routes` or
`publish`. Its input is the rendered `task`, and it must end in `finish`.

`tool_resources.<tool>` holds:
- **`bind`**: the tool's parameters the gateway fills in, mapped explicitly
  `{<tool parameter>: <source>}`. They are removed from the schema the model
  sees, and the model's value for them is overwritten. A source is one of:
  - `"state.<slot>[.<field>]"`, a slot of this agent's state the model cannot
    write (`"state.verified.customer_id"`);
  - `"route.<name>"`, a value the route that dispatched this agent passes;
  - `{const: …}`, a fixed value.

  A route's own `bind` is `{<name>: "state.…" | {const: …}}`: the values it
  resolves from its caller's state and passes down as `route.<name>`. A
  sub-agent has no `state`, so its tools bind `route.<name>` or `const`, as
  in `tool_resources: { mcp__erp__invoices: { bind: { customer_id:
  "route.customer" } } }`.

  A **subject parameter** is a parameter name that some tool of the agent
  binds from `state.` or `route.`. Any other tool of the agent that declares
  a parameter of that name must bind it too. Otherwise it is not offered,
  and a call to it is refused. A `const` bind fixes a setting and makes no
  name a subject.
- **`permission`**: `always_allow` or `always_ask`, which suspends the call
  for staff approval. Without one, a tool its server marks destructive and
  not read-only asks. `approval_timeout` (default `1h`) bounds the wait, and
  nobody approving is a deny ([#96](#what-96-built)).

**Checks when a spec is validated** (on save, again on publish):
- Every reference must exist and be granted to this agent's principal.
- Every `{slot}` in a template must exist in `state`.
- Every `state.` bind source must be a slot whose provenance cannot be
  `llm`; a route's `bind` cannot use `route.`.
- A route passes exactly the `route.<name>` values its live sub-agent binds,
  no more and no fewer. On publish, the routed sub-agent must declare a
  `finish.schema`.
- Every gate must type-check against the slots (§4).
- The sub-agent graph must be acyclic and at most 3 levels deep.
- A `strict` scope lists at least one topic and has a non-blank `refusal`;
  its `classifier_model`, when set, must be granted. Checked on every save, not only on
  publish, because the test chat runs the draft.

### The typed spec (#107)

Runtime code never reads the spec's JSON. It reads `AgentSpec`
(`aiplane-runtime::agents::spec::model`): serde structs and enums for every
part above — `profile`, `scope`, `main` (with `tool_resources`, their `bind`,
`permission` and `approval_timeout`, and `budget`), `state` slots,
`verifiers` (tagged by `kind`), `router`, `routes` (each with exactly one
`RouteTarget`: `agent`, `human`, `a2a` or `loop`), `finish`,
`on_tool_unavailable` and `publish` (origins, `idle_ttl`, `retention_days`,
`rate_limits`, `budget`, `output_filter`, `require_passing_tests`, `a2a`,
`voice`).

- **Validation and typing are two steps over one JSON.** `spec::check` runs
  the path-reporting walk; only when it found nothing does it deserialize the
  same value into `AgentSpec`. The walk keeps the `{path, message}` errors
  (serde would stop at the first and point at a line and column nobody
  typed); the types keep the shape. `spec::validate` is `check` without the
  typed value, so every accepting validator test also proves the two agree:
  a spec the walk passes but the types refuse comes back as an issue at the
  root, which is a bug.
- **Unknown keys are refused twice.** Every type is `deny_unknown_fields`,
  like the walk, so a misspelt or renamed key can never deserialize into a
  silent default.
- **Defaults live in the types.** A field the spec may leave out is an
  `Option` (or an empty collection), and the one accessor that reads it
  applies the default: `Publish::{idle_ttl, visitor_rates, retention_days,
  allows_origin, a2a_enabled}`, `RunBudget::budget`,
  `ToolResource::approval_timeout`, `HumanSpec::{timeout, transcript}`,
  `LoopSpec::max_iterations`, `A2aRouteSpec::seconds`, the `McpCodeSpec`
  and `LookupSpec` accessors and `HostJwtSpec::max_lifetime`. The constants
  they apply stay with the subsystem that documents them.
- **What stays JSON**: a route's `when` (compiled by `gate::Cond` into
  `RouteGates`), `finish.schema` and a `subject` slot's `schema` (JSON
  schemas `FinishContract` enforces), and `state`, which `StateSchema`
  compiles from the same JSON because the walk needs it too.
- **Built once.** `CompiledSpec` (the spec cache) holds the `AgentSpec` of
  each published version beside its `StateSchema`, `RouteGates` and
  `OutputFilter`; `CompiledSpec::agent()` hands it out. A draft run, the
  test-chat debug view and a test case compile the draft the same way.
- **A stored spec that does not read** (only a hand-edited row; every save and
  publish ran `check`) fails loudly instead of falling back field by field:
  it cannot run (`BadSpec`, "it does not read as an agent spec …"), the embed
  endpoint allows it no origin, a visitor admission applies the default
  rates, the retention sweep the default 30 days, and it is not served over
  A2A.

### What #84 built

- **Migration `0077_agent_builder.sql`** creates the three tables above as written.
  Rows live in
  `aiplane-agents::db::agents`; every mutation writes an `agent_audit` row
  in the same transaction (`agent_created`, `agent_draft_updated`,
  `agent_published`, `agent_live_version_set`, `agent_share_set`,
  `agent_share_removed`, `agent_deleted`).
- **Keyed by the principal.** Creating an agent creates its system principal,
  with no grants, in the same transaction. Deleting an agent deletes the
  principal, which cascades to its grants, tokens, versions and shares; the
  audit trail stays. Grants are made through
  `/api/v0/system-principals/{id}/grants` (#77's grant-time cap). For a
  principal that is an agent, those routes also require a share on the agent:
  `read` to see it, `write` to change grants, tokens or disable it. The list
  there hides agents the caller holds no share on.
- **Validator** (`aiplane-runtime::agents::spec`). It walks the JSON and
  returns every problem as `{path, message}` rather than stopping at the first
  one. Two stages:
  - *Draft* (on save and on create): shape and references. Unknown keys at any
    level, types, enums, durations (`30s`/`15m`/`2h`/`30d`), exact origins,
    regexes, the `finish` schema (through `FinishContract::new`). Models, tools,
    MCP tools (via their connector), skills and verifier connectors must be
    granted. Sub-agents must be existing agent ids, and not the agent itself.
    Gate leaves, templates and `set_by: verifier:<id>` must name declared slots
    and verifiers. Bind sources must be non-`llm` slots or `const`. Gates are
    checked for shape only.
  - *Publish*: everything above against the grants as they are now, plus
    instructions, every routed sub-agent having a live version, and a granted
    model for every model key left unset: `main.model` and each voice
    direction that is on run on the gateway's default of their kind
    (`SpecContext::model_defaults`, [Models](#models)), so that default must
    exist and be granted.
  - Slot run-time semantics landed with #85 ([below](#what-85-built)), gate
    type checks with #86 ([§4](#what-86-built)), the sub-agent graph checks
    with #88 ([§3](#what-8788-built)).
- **Shares.** The holder must have `can_manage_agents` when the share is
  written. For a user that means through their groups; a group needs the flag
  or `is_admin`, asked of the RBAC resolver, so a group from `[rbac]` config
  or the bootstrap admin group counts as one from the database does. The
  caller must also hold the permission on every request.
  **One rule** decides who may act on an agent:
  `aiplane_runtime::agents::access::effective_access` — `write` for an
  admin; otherwise the strongest share, but only while the person holds
  `can_manage_agents`; otherwise nothing. The `/api/v0/agents` routes, the
  inbox's manager standing and the `a2a_caller` grant cap all ask it, so a
  manager who loses the permission loses every agent with it, whatever
  shares are left behind.
  Without a share, an agent answers 404, not 403. Removing or downgrading the
  last `write` share is refused (`409 last_writer`). Admins implicitly hold
  `write` on every agent without a share: they see it in the list and can read,
  edit, publish, share and delete it. An agent whose last writer left therefore
  stays recoverable. Non-admin managers still need a share.

**API** (`aiplane-api::pages::json_agents`). Every route needs a session with
`can_manage_agents`. `read`/`write` is the share needed.

| Method | Path | Share | Purpose |
|---|---|---|---|
| GET | `/api/v0/agents` | any | Agents shared with you (every agent for an admin), with your `access` |
| POST | `/api/v0/agents` | — | Create `{name, display?, description?, spec?}`; 201, 409 on a taken principal name |
| GET | `/api/v0/agents/{id}` | read | Agent, `draft_spec`, `live_spec`, `publish_issues`, grants, shares, audit |
| PUT | `/api/v0/agents/{id}/draft` | write | Replace the draft `{spec}`; the live version is untouched |
| POST | `/api/v0/agents/{id}/publish` | write | Validate the draft for publishing, snapshot it as version n+1, make it live |
| GET | `/api/v0/agents/{id}/versions` | read | Every version, newest first, with its spec |
| POST | `/api/v0/agents/{id}/live` | write | `{version}`: rollback (or forward); not re-validated, since grants are not versioned |
| GET | `/api/v0/agents/{id}/shares` | read | The shares |
| POST | `/api/v0/agents/{id}/shares` | write | `{subject_kind: user\|group, subject_id, access: read\|write}`; 422 `share_needs_agent_manager` if the holder lacks the permission |
| POST | `/api/v0/agents/{id}/shares/revoke` | write | `{subject_kind, subject_id}` |
| DELETE | `/api/v0/agents/{id}` | write | Delete the agent and its principal |

An invalid spec is `422` with `error.code = "invalid_agent_spec"`. The
`message` names the first problem; `error.issues` lists all of them. Each
ungranted reference says which grant to make, for example `POST
/api/v0/system-principals/{id}/grants {"kind": "tool", "ref": "rag_search"}`.

### What #90 built

The builder UI (`/agents`, [`ui.md`](ui.md#agent-builder)) and the internal
test chat behind it.

- **`POST /api/v0/agents/{id}/test-turn`** (`write` share) `{message,
  session_id?}`. It runs one message against the agent's **draft** through the
  ordinary run path (`agents::run::run_turn_with`), as the agent's principal:
  its grants, gates, binds and budgets apply and its tools really run. No
  `session_id` starts a conversation; one continues it. The turn is synchronous
  (the request returns when the turn ends; no SSE yet). Answer:
  `{session_id, turn_id, status, answer, error, draft_version: 0, debug}`.
  A turn that paused also carries `suspension`
  ([agent-run suspend](#what-agent-run-suspend-built)).
  Failures: `404 unknown_session`, `422 agent_not_runnable` (bad spec),
  `503 agent_no_model` (the spec names no model and the gateway has no default
  chat model), `422 agent_model_not_granted` (the agent holds no grant on the
  model it runs on).
- **`debug`** is for managers only and is built from the stored state and the
  turn's `agent_audit` rows, never from anything a visitor can reach:
  - `slots`: per declared slot `{slot, status: set|missing|invalid, value,
    provenance, set_at, set_by, reason?}`. Unlike the model's view, `value` is
    shown for verifier and host slots too.
  - `routes`: per route `{route, description?, open, missing: [Unmet]}`,
    judged on the state after the turn.
  - `routing`: this turn's `route_decision` details (every gate, the route
    picked).
  - `sub_agents`: this turn's `sub_agent_dispatched` details, replaced by the
    matching `sub_agent_finished` (with `outcome`) once it ended.
  - `tool_calls`: this turn's `tool_call` decisions (`allowed`/`denied` and
    the policy).
- **How the draft is run.** `RunProfile::load_from(state, id, SpecSource,
  role, options)` takes the spec explicitly: `Live`, `Pinned(version)` or
  `Draft(spec)`. `load` and `load_version` are the first two, and only
  `agents::run::draft::run_draft_turn`, called by the test-turn handler,
  passes `Draft`. It opens the session itself and then uses the same
  `drive_opened` as a visitor's turn, so nothing about what "live" means is
  overridden and no other path can reach a draft. The draft is recorded as
  version `0` (`DRAFT_VERSION`): a test session is never continued as a
  visitor's (`MissingVersion`), nor a visitor's as a test (`unknown_session`).
  A sub-agent the draft dispatches to loads its own live version. The
  `embed` integration test publishes v1, edits the draft, and shows the test
  chat running the draft while a visitor message gets v1.
- **`GET /api/v0/agent-resources`** lists what the calling manager holds and
  can therefore grant: `{models: {chat, transcription, speech}, defaults,
  tools: [{id, name, description}], connectors: [{key, name, tools}], skills,
  rag_collections: [{id, name}]}` ([Models](#models)). It applies the
  same predicates as the grant route's cap, so the builder's pickers never
  offer what `POST …/grants` would refuse with `grant_exceeds_manager`.
- **`GET /api/v0/me`** gained `can_manage_agents`; the SPA shows the Agents
  section only when it is true.

## 3. Run model

### Where runs live

Agent conversations reuse the chat substrate: `chat_sessions` and `chat_turns`,
the worker registry, the `chat_json` SSE protocol and compaction. They are owned
by the principal, not by a person.

```sql
-- table rebuild; SQLite cannot alter a foreign key in place
chat_sessions:
    user_id       TEXT NULL REFERENCES users(id) ON DELETE CASCADE
    principal_id  TEXT NULL REFERENCES system_principals(id) ON DELETE CASCADE
    visitor_id    TEXT NULL REFERENCES visitor_sessions(id) ON DELETE SET NULL
    parent_turn_id TEXT NULL            -- set on a sub-agent run: the main turn that called it
    agent_version INTEGER NULL
    CHECK ((user_id IS NULL) != (principal_id IS NULL))
```

- **Separation from people.** Every existing query lists sessions by `user_id`,
  so agent conversations never show up in anyone's chat list without extra code.
  The owner sees them only through the agent's conversations view, which
  requires a share.
- **Sub-agent runs** are child sessions owned by the sub-agent's principal and
  linked through `parent_turn_id`. They are not chats of the owner, as decided.

**As built (#83, the `chat_sessions` rebuild in migration `0077_agent_builder.sql`).** The
rebuild is as above, with `visitor_id` pointing at #91's `visitor_sessions`
([§5](#what-91-built)). A second CHECK, `principal_id IS NULL OR shared = 0`,
keeps an agent conversation out of the "anyone with the link" read path.
`parent_turn_id` has no foreign key: the child run stays an auditable record
when the parent turn is gone. `SessionOwner`, `create_principal_session`
(`NewRunSession`), `get_principal_session` and `session_owner` landed in
`session_core::db` and moved to `aiplane_agents::db::run_sessions` in #109,
which left session-core owner-agnostic: every person-facing query is
unchanged and simply never matches a row whose `user_id` is `NULL`, and
session-core never names the other owner. The chat sweeper for expired
suspensions skips principal-owned runs; the agent path resumes those
([agent-run suspend](#what-agent-run-suspend-built)). Because seven tables cascade from `chat_sessions`, migrations now run
with foreign keys off — see `crates/aiplane-core/migrations/README.md`.

### State

```sql
CREATE TABLE agent_state (
    session_id  TEXT NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
    slot        TEXT NOT NULL,
    value       TEXT NOT NULL,      -- JSON, already validated
    provenance  TEXT NOT NULL,      -- 'llm' | 'verifier:<id>' | 'host'
    set_at      TEXT NOT NULL,
    PRIMARY KEY (session_id, slot)
) STRICT;
```

- **Who can write a slot.** Only the slot's `set_by` list:
  - `set_<slot>` tools exist only for slots that include `llm`.
  - Verifiers and the host-JWT path write their slots in code.
- **What the model sees.** Every round, the system message states each slot as
  `set`, `missing` or `invalid: <reason>`, never the value of a verifier slot.

### Synthetic tools

Besides its granted tools, a run gets generated tools. They are always
registered for that run, need no grant, and do not exist anywhere else.

| Tool | Offered to | Effect |
|---|---|---|
| `set_<slot>(value)` | main agent | validates in code and writes `agent_state` with provenance `llm` |
| `forward_request()` | main agent | evaluates the router over open gates (§4). On no open route it returns the failing conditions as a structured list. Otherwise it runs the sub-agent and returns its `finish` result |
| `request_human(question)` | main agent, when a `human` route exists | hands the conversation to a person on an open human route and waits for their answer ([#96](#what-96-built)) |
| `verify_<id>_request_code()`, `verify_<id>_submit_code()`, `verify_<id>()` | main agent | the verifier flows ([#95](#what-95-built)); no arguments |
| `finish(result)` | agent runs under a finish contract (routed sub-agents) | ends the run. `result` is checked against the finish schema |

**Order within a round.** A model batches calls, so `set_issue` and
`forward_request` often arrive in one round. A round's calls normally run
concurrently, which would let the forward read the state before the write
landed. Each synthetic tool is therefore tagged with a `ToolPhase` where the run
builds it (`AgentSurface` in `agents/profile.rs`; the run's `finish` tool is
the one other tool that leaves the default), and the runner
(`execute_tool_calls`) runs the phases in turn:

1. **Writes state**: `set_<slot>` and every verifier tool, one at a time in the
   order the model made the calls (a lookup reads the slots a `set_<slot>`
   before it wrote).
2. **Concurrent**: every other tool, in parallel as before. A bound argument may
   be read from state, so these too run after the writers.
3. **Acts on state**: `forward_request` and `request_human`, one at a time in
   call order, on the state the round left.
4. **Terminal**: `finish`. The driver runs it only as the one call of its
   round, and a call that succeeds ends the run ([finish
   contract](tools-rbac.md#finish-contract)).

Each result still answers its own `tool_call_id`, in call order. Outside an
agent run every tool is `Concurrent`, so chat and `/v1` are unchanged.

`forward_request` takes **no arguments** about the route or the subject.
- The router decides from state.
- With `kind: classifier`, a separate small call returns one route name from an
  enum. It can only pick among routes whose gate is already open.

### What #85 built

- **Migration `0077_agent_builder.sql`** creates `agent_state` as above, plus a
  `CHECK` that `provenance` is `llm`, `host` or `verifier:<id>`. It references
  `chat_sessions(id)` only, so it holds for a user-owned and a principal-owned
  session alike and does not depend on #83's `chat_sessions` rebuild. Storage is
  `aiplane-agents::db::agent_state` (`put`, `for_session`); it knows
  neither types nor writers and has one caller.
- **Typed slots** (`aiplane-runtime::agents::state`). `StateSchema::from_spec`
  reads `state` into `SlotDef`s. `SlotDef::check` validates in code and returns
  what to fix:
  - `string`: `min_length`, `max_length` (characters), `pattern`.
  - `email`: `local@domain.tld`, no whitespace; `max_length`.
  - `enum`: one of `values`.
  - `integer`, `number`: `minimum`, `maximum`.
  - `boolean`.
  - `subject`: an object, checked against an optional `schema` by #78's
    schema-subset validator (`finish::validate`).
- **Two write doors.** `set_<slot>` always writes provenance `llm`.
  `write_trusted(pool, schema, session, slot, value, TrustedWriter, now)` is
  the only way to store `verifier:<id>` or `host`; `TrustedWriter` has no
  conversion from a string or JSON, so a value parsed from a tool call cannot
  become one. Both refuse a writer outside `set_by` before looking at the
  value, then refuse an invalid value with the validator's message. A rewrite
  replaces the row, provenance and `set_at` included.
- **`set_<slot>` tools** (`agents::slot_tools`). `SlotTools` is a `ToolSource`
  with one `SetSlotTool` per slot whose `set_by` lists `llm`, and none for any
  other. A tool takes exactly `{value}` (`additionalProperties: false`); any
  other key, such as `slot`, `provenance` or `session_id`, is refused by name.
  The session comes from `ToolContext.session_id`. Invalid values come back as
  `InvalidArgs` ending in "call set_<slot> again".
- **The model's view.** `AgentState::load` judges each stored row against the
  current schema: `set`, `missing`, or `invalid` (a writer the spec no longer
  allows, or a value that no longer fits). `view()` returns `SlotView`s and
  `render_view()` the system-message block. A value is shown only when its
  provenance is `llm`; a trusted slot says `set by verifier:otp`, and its
  invalid reason never echoes the value.
- **Clock.** Writes take `now`, and `SlotTools::with_clock` injects it, so
  tests fix `set_at` and every `max_age` gate on top of it.
- **Spec checks added.** A constraint on a type it does not apply to (e.g.
  `pattern` on an `integer`) is an error, not silently unenforced. `schema` is a
  new slot key for `subject` slots only. A `subject` slot may not list `llm`:
  it says whose data the agent acts on.
- **Wired by #87/#88.** An agent run offers `SlotTools` and puts
  `render_view()` into its system message every round
  ([below](#what-8788-built)). There is no `state` SSE event yet.

### The call chain

```rust
pub struct RunChain {
    pub root_session: String,           // visitor conversation
    pub visitor_id: Option<String>,
    pub frames: Vec<Frame>,             // main agent, then each sub-agent hop
}
pub struct Frame { pub principal_id: String, pub version: i64, pub via_tool_call: Option<String> }
```

`RunChain` rides in the run's `AgentRun` (`ToolContext::chain()`). These records carry the serialized chain:
- every `mcp_tool_audit` row,
- every usage row,
- a new `agent_audit` table (columns `kind`, `principal_id`, `chain`, `detail`,
  `created_at`).

`agent_audit.kind` covers gate decisions, route picks, sub-agent dispatch and
finish, verifier outcomes, grant changes, output-filter hits and injection
flags. Depth is capped at 3, checked again at run time as well as at validation.

**As built (#83).** `RunChain` lives in `aiplane-core` (`server/run_chain.rs`)
next to `Principal`, because the audit rows that serialize it are written
there. Two changes from the sketch above: a `Frame` also carries the
principal's `name`, so an audit row reads without a join and outlives the
principal; and `via_tool_call` is `via: Option<CallSite { turn_id,
tool_call_id }>`, because the child session's `parent_turn_id` needs the turn,
not only the call. `version` is `Option<i64>` until agent versions exist (#84).
`RunChain::root` starts a chain, `enter` appends a sub-agent and refuses a
fourth level (`EnterError::TooDeep`). Since #88 it also refuses an agent
that is already in the chain (`EnterError::Cycle`).

- **One `AgentRun` value** (`aiplane_runtime::agent_run`). Everything that sets
  an agent's run apart from a person's turn travels together: the
  `SystemPrincipal` it acts as, its `Arc<RunChain>`, its finish contract,
  budget and injection scan, and the spec's `AgentSurface`. `AgentRun::new(
  principal, chain)` is the only constructor and returns `MismatchedRun` when
  the chain's running frame is not that principal, so a run that acts as one
  agent and audits as another cannot be built; `with_contract`,
  `with_budget`, `with_injection` and `with_surface` add the rest. A turn's
  `Actor` is `Person { id, roles }` or `Agent(Arc<AgentRun>)`: `DriveParams`
  and `TurnFacts` take one, and `build_tool_context` derives
  `ToolContext.principal` from it, so the principal and the run cannot
  disagree. The run then lives in `ToolContext.agent: Option<Arc<AgentRun>>`
  (the driver reads it back through `OpenAiDriver::agent()`), and every
  question "is this an agent run" asks that one value: `agent_active()`, the
  call policy, the injection audit, the usage row's chain (`ctx.chain()`), and
  whether `headless::drive` announces a person's pause.
- **Every tool call in an agent run is decided and audited** in
  `openai_driver/call_policy.rs`: one `agent_audit` row of kind `tool_call`
  per call, attributed to the running principal, `actor_id` `NULL`, `chain`
  set, `detail` `{tool, call_id, turn_id, session_id, decision, policy}`.
  `decision` is `allowed` or `denied`; `policy` is `granted`,
  `not_granted`, `unknown_tool` or `disabled_in_conversation` (a person's run
  can also be `auto_enabled`, but a person's run writes no audit rows).
- **A system principal never auto-enables.** The chat path runs a registered
  tool the model calls without its schema and turns it on for the
  conversation. For a system principal that call is refused as
  `not_granted`: it holds exactly its grants.
- Run events share one writer, `agent_audit::record_run_event(principal,
  chain, detail)`: tool-call decisions and #93's `injection_detected`
  findings both carry the chain when the call is inside an agent run.
- `mcp_tool_audit` has a `chain` column (migration `0077_agent_builder.sql`), set on every MCP
  call inside an agent run. Since #92 usage rows carry it too, plus the
  main agent as `agent_id` ([§5](#what-92-built)).
- An agent run gets none of its owner's identity, memory, private skills or
  MCP connections: the request context reads the user row, memories and
  private skills only through `Principal::user_id()`, which is `None` for a
  system principal. Usage rows from such a run carry `principal_kind =
  'system'` and the principal's name.

### Sub-agent dispatch

`AgentToolSource` is a dynamic `ToolSource`, the same mechanism MCP and ComfyUI
use. It lives in `aiplane-runtime` and exposes the synthetic tools above. When a
route opens, `forward_request` does five things:

1. Loads the sub-agent's live version and its principal's grants.
2. Renders `task` from state and resolves `bind` values from state.
3. Opens a child session under the sub-agent's principal, with the task as the
   user turn.
4. Drives it through the same driver with a `RunProfile` (below), awaiting it
   inside the parent's tool call. Its own budget is independent of the
   parent's.
5. Returns the `finish` JSON, or `incomplete`, as the tool result. The main
   agent treats it as data.

### `RunProfile`: one loop, several shapes

`OpenAiDriver` gets a `RunProfile` instead of a second driver:

```rust
pub struct RunProfile {
    pub system: SystemMessage,          // Chat (today's leading_system_message) | Agent { spec, state }
    pub budget: Budget,                 // { rounds, seconds, tokens }; Chat derives it from Effort
    pub finish: Option<FinishContract>, // Some => run ends only via finish() or incomplete
    pub output: OutputPolicy,           // Stream | Buffered { filter }  (public main agents)
}
```

**Agent system message.** It is the agent's `orchestration` and `response`
instructions plus the slot status and the granted skill listing. It has no
built-in chat rules, no user memory, no location and no MCP listing beyond the
grants.

**The finish contract.**
- A sub-agent that writes text without calling `finish` gets one nudge.
- Hitting any budget limit takes the existing final-round path. That yields
  `{"status":"incomplete","reason":"rounds|seconds|tokens|repeated_call|tool_unavailable","done":"…"}`.
- An invalid `finish` payload goes back to the model with the validator's error
  and counts as a round.

**Phase-0 loop items** (they also benefit chat and scheduled actions):
- **Intra-turn trimming (#80):** past a context threshold, old tool results in
  the replayed tail become `[result trimmed: <first 300 chars>]`. The stored
  turn keeps them in full.
- **Repeated calls (#81):** the third call in a turn identical in
  `(tool, canonical JSON args)` gets a tool error. A fourth ends the run as
  `incomplete: repeated_call`.

### What #87/#88 built

`aiplane-runtime::agents::{profile, router, bind, run}`.

- **Entry point.** `agents::run::run_turn(state, AgentTurn { agent_id,
  session_id, message, visitor_id })` runs one visitor message as the agent's
  principal and returns `AgentReply { session_id,
  turn_id, status, answer, error }`. A new conversation is a principal-owned
  session (`parent_turn_id` `NULL`, `agent_version` set). A `session_id` must
  belong to that agent, or the call fails with `UnknownSession`; a new
  conversation runs the live version, a continued one the version it started
  on (#91, `RunProfile::load_version`). The chain is
  `RunChain::root(session, visitor, main frame)`. `run_turn_with` takes
  `RunOptions { now, classifier }`: the clock that gates and slot writes read,
  and a `RouteClassifier` to use instead of the model classifier. The public
  endpoint (#91) drives its turns through the same `drive_opened`; the test
  chat is #90.
- **`RunProfile::load(state, agent_id, Role, options)`** reads the live version
  and the principal (`load_active`, so a disabled one is refused). It returns
  `{principal, version, model, budget, finish, injection, surface:
  Arc<AgentSurface>}`. `agent_run(chain)` builds the run's `AgentRun` from it,
  and `drive_params` wraps that in `DriveParams { actor: Actor::Agent(..) }`;
  both fail with `MismatchedRun` for a chain whose running frame is another
  agent. The `AgentRun` rides the tool context into the ordinary headless
  loop. There is no second driver: the round loop asks the turn's
  `TurnPolicy` (`openai_driver/turn_policy.rs`, `Chat` or `Agent(&AgentRun)`)
  for the system message, the offer, the model access, the budget and how the turn
  ends, and holds no agent branch of its own. An agent run therefore never
  reads a person's conversation overlay or "off" switches
  (`chat_session_tools`); a run with no spec surface gets the turn-discipline
  rule as its system message and every grant as its offer.
  - *Compiled once*: a published version is immutable, so its typed spec
    ([`AgentSpec`](#the-typed-spec-107)), `StateSchema`, `RouteGates` and
    `OutputFilter` are built once per
    `(agent, version)` and shared (`agents::spec_cache`, at most 256, least
    recently used first). Which version is live is read fresh for a run and
    held for 5 s on a visitor admission.
  - *Model*: `main.model`, else the gateway's default chat model
    (`agents::defaults::main_model`). The principal must hold a `model` grant
    on it (`ModelNotGranted` otherwise); the run's access is narrowed to it
    (`PoolAccess::for_system_models`). An automatic-route alias is resolved
    per turn exactly like a person's chat turn (`server::model_route`).
  - *Budget*: `main.budget`, with rounds defaulting to the `standard` effort
    cap.
  - *Finish*: none for the main agent, which ends its turn with text. A
    sub-agent's comes from its `finish.schema`. There is no default: the
    validator requires the schema when publishing a route to the agent, and a
    dispatch to one without it fails with `BadSpec`.
  - *Injection*: `Flag` for both.
- **System message** (`AgentSurface::system_message`). It holds a line naming the
  agent, `orchestration` and `response`, `render_view()`, one line per route
  (`- billing (description): open` or `closed — <each unmet message>`), and
  the compaction summary if any. It has no chat rules, no request context, no
  memory and no connector listing. When the agent has state or routes it is
  rebuilt before every round after the first, so a slot set in round *n* shows
  in round *n+1*.
  Everything that reads the state during a turn (the system message, gates,
  bound arguments, `forward_request`, `request_human`, the output filter)
  shares one `StateSnapshot`, read again only after a `WritesState` call ran.
- **Tools** (`RunToolSource`, the `AgentToolSource` of the sketch above;
  [`tools-rbac.md`](tools-rbac.md#tool-sources)).
  The run offers the spec's `main.tools` that are also in the principal's grant
  (`AppState::granted_tool_ids`), then `set_<slot>` per model-writable slot,
  then `forward_request` when the spec has routes. The synthetic tools sit
  over the `GrantedToolSource`. The call policy sees them as offered, so each
  call is decided and audited as `granted`. A granted tool missing from
  `main.tools` is not offered, and a call to it is refused as `not_granted`.
- **Router** (`forward_request()`, #87). It takes no arguments, and any key is
  refused by name. It evaluates every gate and works only with the open
  routes:
  - *No open route*: the result is `{forwarded: false, reason:
    "no_open_route", routes: [{route, missing: [Unmet]}]}`.
  - *`rules` router*: the first open route in `router.order`, a new optional
    key with route names, else the first open route by name. Deterministic.
  - *Exactly one open route*: that route, without asking a model.
  - *Otherwise* (`classifier`, or no router): a non-streaming call on
    `router.model`, else the main run's model. It uses `response_format: json_schema`
    with `route` constrained to `enum: <open routes>`, and sends the route
    descriptions and the model's slot view, never trusted values. The answer
    is checked in code again, so a closed, unknown or malformed answer
    forwards nothing (`no_route_chosen`).
  - Dispatch happens only through the `OpenRoute` that `RouteGates::open`
    returns. A `human` route hands the conversation to a person
    ([#96](#what-96-built)).
- **Dispatch** (#88). It is the five steps of
  [Sub-agent dispatch](#sub-agent-dispatch):
  1. Render `task` from the main agent's state with `bind::render_task`. A
     placeholder may read any valid slot, verifier slots included: the task
     goes to the sub-agent, not back to the visitor or the main model. A
     placeholder that cannot be filled answers `task_incomplete`.
  2. Resolve the route's `bind` against the state.
  3. Load the sub-agent's profile with those values and enter the chain
     (`CallSite { turn_id, tool_call_id }`; the call id is
     `ToolContext::call_id`, which the runner sets on each call's context).
  4. Open a child session owned by the sub-agent's principal
     (`parent_turn_id` = the main turn) and `drive` it with its own budget,
     contract, grants and injection policy.
  5. Return `{forwarded: true, route, sub_agent, outcome: RunOutcome, note}`
     as the tool result. The main agent's `Flag` policy screens it.

  The sub-agent sees its system message and the task, never the transcript.
  The call waits at most 15 minutes.
- **Bound arguments** (`bind::BoundTool`). A tool binds exactly the
  parameters its `tool_resources.<tool>.bind` maps; nothing binds by name.
  `route.<name>` sources are filled from the dispatching route's values.
  Mapped parameters are dropped from `properties` and `required`, and the
  gateway's value overwrites the model's. A `state.` bind is read at call
  time and refuses the call while the slot is unset. A tool that declares a
  subject parameter it does not bind is withheld (`bind::WithheldTool`): it is
  not offered, and a call to it is refused.
- **Cycles and depth.** `RunChain::enter` returns `EnterError::{TooDeep,
  Cycle}` at run time. The validator walks the live specs of every agent
  reached (`SpecContext.live_specs`, from `agents::live_specs`). A route whose
  graph loops back, or nests more than 3 agents counting this one, is an issue
  at `routes.<r>.agent`.
- **Gate rule completed.** A route needs a trusted gate when it binds from
  state, or when its sub-agent, or anything below it, has a `bind`. A route
  that binds from slot `s` must also gate on a non-`llm` provenance *of `s`
  itself* (`Cond::requires_trusted_provenance_of`).
- **Audit.** New `agent_audit` kinds, written on the calling principal with
  its chain:
  - `route_decision` (`{routes: [{route, gate}], picked, reason?}`)
  - `sub_agent_dispatched` and `sub_agent_finished` (`{route, sub_agent,
    sub_agent_id, version, session_id, turn_id, outcome?}`)

  The sub-agent's own tool calls carry the extended chain.
- **Deviations.**
  - `main.skills` is not listed in the system message, and `read_skill` is
    offered only if granted and listed in `main.tools`.
  - Usage rows of the classifier call were not written; #92 added them.
  - Whether a tool *declares* a subject parameter is known only from its
    schema, and MCP schemas exist only once the connector is connected. The
    validator therefore checks the route ↔ sub-agent contract (`route.`
    names, finish schema). The unbound-subject rule is enforced at run time,
    against the real schema, by withholding the tool.
  - The output policy (`Stream`/`Buffered`) is not part of the profile yet:
    #89 filters the finished answer in `run_turn`; the buffering itself
    arrives with the public endpoint (#91).
- **Tests.** `agents/run/tests.rs` runs the support example end to end on
  wiremock upstreams, both models and the ERP as a wiremock MCP server:
  1. The closed gate's feedback.
  2. A verifier write through `TrustedWriter`.
  3. `forward_request` dispatches billing.
  4. The bound `customer_id` overrides the model's `K-99999`.
  5. The finish result comes back to the main agent, which answers.

  More tests cover: a closed route the classifier names; `order` in the rules
  router; separate budgets; a sub-agent that routes back to its caller; an
  injection in a sub-agent result; and the audit chain.

### What #89 built

`agents/output_filter.rs` is the whole filter; `run_turn` calls
`guard_answer` once, on the main agent's final answer.

- **Spec.** `publish.output_filter.patterns` (name → regex, validated at
  publish) and `publish.output_filter.action` (`withhold`, the default, or
  `redact`). No patterns, no filter: the answer is untouched.
- **Rule.** Every match of every pattern in the answer must also occur, as
  the same pattern's match, in the turn's *trusted text*. Anything else, the
  visitor's own message included, is untraceable. Trusted text is:
  - the conversation's slots not written by `llm`;
  - the outputs of the turn's **successful** tool calls. An errored call
    contributes nothing: its message is the tool talking about its input
    ("no invoice RE-99999 found"). *Errored* is the call's real outcome: the
    runner marks a result `failed` (`ToolResultRecord::failed`) when the tool
    returned an error (an MCP `isError` included), rejected its arguments,
    timed out, was unregistered, or never ran (refused as a repeat, over the
    budget, a second suspend request in a round), and the driver stores
    those rows as `errored` — which is also what the chat UI shows as a
    failed call. The `set_<slot>` calls are left out because they echo
    model-written values.
  - *Echo rule.* An identifier in a call's output does not vouch for itself
    when that call's model-supplied argument values could have supplied it:
    the model chose it, the data did not. Otherwise the model could launder a
    visitor's claim by passing it to a tool that repeats it. Both sides are
    compared as lowercase letters and digits only, so reformatting does not
    hide an echo (`{"invoice": 999999}`, `"re 999 999"` or `"RE-"` +
    `"999999"` all supply `RE-999999`). An identifier with digit runs of at
    least 4 digits is supplied when *every* such run occurs in some argument:
    the runs tell one customer from another, a prefix is the pattern's. One
    without such runs is supplied when its whole alphanumeric core occurs in
    one argument. Shorter runs are ignored because they occur in almost any
    argument by chance, so a lookup by `{"year": 2026}` still vouches for the
    `RE-2026-0042` it returned. *Chosen over* a pattern's capture group: it
    asks every spec author to mark the distinctive part, and a spec without
    one would fall back to the weaker verbatim test. The stored arguments
    are the model's own; a bound argument is filled in by the gateway
    afterwards, so a tool repeating a bound value is not an echo (and the
    value came from a verified slot, which is trusted on its own anyway).
  - *Sub-agents.* A `forward_request` result is not trusted text: a sub-agent
    repeats its task (which can carry an `llm` slot) as readily as a model
    repeats a visitor. Instead the successful tool calls of every sub-agent
    run below the turn (`chat_sessions.parent_turn_id`, recursively, loop
    workers and critics included) count by the same rule. A sub-agent's
    answer can thus name an invoice its own lookup returned, but not one it
    only read in its task. *Chosen over* trusting the identifiers of the
    sub-agent's finish result that also occur in those tool outputs: that
    only narrows trust to what the sub-agent chose to mention, which adds no
    safety, and walking the tool calls needs no second notion of "the result".
  - *People.* A staff answer to a handoff (`{answered: true}` from
    `forward_request`'s human route or `request_human`) is trusted whole:
    a person read the request and wrote it.
- **Withhold** replaces the whole answer with the `agent-output-withheld`
  catalog message; **redact** replaces each offending identifier with
  `agent-output-redacted`. The stored turn is overwritten the same way, so a
  replayed conversation never shows the blocked text.
- **Audit.** `output_blocked` on the agent's principal with the run chain and
  `{action, session_id, turn_id, patterns, original, delivered}`. `patterns`
  holds the pattern name of each offending occurrence; since #111 the
  activity log also keeps the withheld `original` answer, for the agent's
  managers only ([below](#what-111-built)). If the trusted text cannot be
  read the answer is withheld (fail closed) and the row carries `error`.
- **One call site, both entry points.** `drive_opened` runs the filter, so
  `run_turn` and the public endpoint's runner are both covered.
- **Language.** The fallback text is in the conversation's recorded language
  (`chat_sessions.lang`, [Suspend and resume](#suspend-and-resume-82)):
  `AgentTurn.lang` for `run_turn` (English when unset), the request's
  `Accept-Language` for a visitor message.
- **No early peek.** The turn row is terminal before the filter has ruled, so
  the embed endpoint treats a session as unfinished while its claim (a
  worker in the session worker registry) is still held: the snapshot shows
  the turn in progress and the event stream waits for the worker's
  `Released`.
- **Limits.** The filter matches text, not meaning: an identifier the model
  rewrites (`RE 123456`) escapes a pattern that does not allow for it, and a
  tool the agent calls that returns another customer's data makes that data
  trusted. Grant tools bound to the verified subject (#88) for that. The echo
  rule errs towards withholding: a tool that returns an identifier whose
  digit runs merely happen to occur in its arguments (an invoice numbered
  like the customer) does not vouch for it. A tool that answers an error as
  a successful result (`{"error": …}` without failing) is not recognised as
  failed; the echo rule still catches the number the model passed it. A
  successful lookup by a number the visitor gave
  does not confirm that number either, only what the lookup returned beside
  it. Only this turn's calls count: an identifier a tool returned in an
  earlier turn must be looked up again. An A2A route's remote answer is never
  trusted text.
- **Tests.** `agents/run/tests.rs` runs the support example with a filter:
  another customer's invoice is withheld and audited; identifiers from the
  verified slot and the sub-agent's lookup pass; redaction; no patterns leaves
  the answer unchanged. `agents/run/tests/output_filter.rs` covers the echo
  rule end to end: an errored lookup that names the visitor's number, a
  failed lookup whose error names another invoice, a lookup that formats the
  model's bare number as an invoice, a lookup by customer whose invoice
  passes, an echo tool, a lookup that vouches only for what it found, and a
  billing sub-agent
  whose looked-up invoice passes while the number it only read in its task is
  withheld. Pure cases are in `output_filter.rs`.

### Suspend and resume (#82)

`FeedbackHub` parks a call only while the turn lives in memory. That is enough
for `ask_user` and browser control, not for an approval that may take hours.

```sql
CREATE TABLE chat_turn_suspensions (
    turn_id      TEXT PRIMARY KEY NOT NULL REFERENCES chat_turns(id) ON DELETE CASCADE,
    kind         TEXT NOT NULL,          -- 'approval' | 'secure_input' | 'human_answer'
    tool_call    TEXT NOT NULL,          -- JSON {id, name, args}
    tail         TEXT NOT NULL,          -- JSON: this turn's round messages so far
    budget_used  TEXT NOT NULL,          -- JSON {rounds, seconds, tokens}
    child_turn   TEXT,                   -- set when the suspension is inside a sub-agent run
    expires_at   TEXT NOT NULL,
    created_at   TEXT NOT NULL
) STRICT;
```

**Suspending**
- The runner writes this row and sets `chat_turns.status = 'suspended'`.
- It emits a new `chat_json` event `suspended { kind, request_id, options }`
  and frees the worker.
- A suspension inside a sub-agent also suspends every ancestor turn. Their rows
  point at the child through `child_turn`.

**Resuming**
- `POST …/turns/{id}/resume { decision: allow_once | deny | value }` re-enters
  the loop with the `tail` and the decision as the tool result.
- It continues at the innermost turn first, then each parent.

**Expiry**
- A sweeper turns an expired suspension into `deny` with reason `timeout`.

**As built (#82).** The table also carries `request_id` (so an answer to an
earlier pause of the same turn cannot settle a later one), `message` (what the
tool wants shown) and `on_timeout` (`deny` | `allow_once`, the configurable
fallback; deny by default). `expires_at` is compared after parsing, not as a
string. The resume route for the chat path is
`POST /api/v0/chat/sessions/{id}/turns/{turn_id}/resume`, owner-only, with
`{decision, value?, request_id?}`. Agent runs got their own resume path
afterwards ([below](#what-agent-run-suspend-built)). Details in
[`tools-rbac.md`](tools-rbac.md#suspend-and-resume).

### What agent-run suspend built

The shared prerequisite of #95 (secure input) and #96 (human in the loop):
agent runs pause and resume durably, sub-agent runs included.

- **Kinds.** `session_core::db::SuspensionKind` is the registry: `approval`,
  `secure_input`, `human_answer`. Each kind says which decisions it offers
  (`options()`), who answers it in an agent conversation (`answered_by()`:
  `Participant` for `secure_input`, `Staff` for the other two) and what its
  timeout falls back to (`timeout_fallback()`). The driver applies that
  fallback when it pauses, whatever the tool asked: an approval only ever
  times out to `deny` (#96's decision), and a value kind cannot time out to
  `allow_once`, which it does not offer. So every expiry today is a deny.
  `SuspendRequest::approval(timeout)` and `SuspendRequest::secure_input(message,
  timeout)` build the requests; #95/#96 need no new kind.
- **Which runs pause.** `ToolContext.suspend` is `Available` on the chat path
  and on every agent run (`headless::drive` turns it on when `DriveParams.actor`
  is an agent), in both `drive_opened` entry points: the public endpoint's runner
  and the test chat. Since #96 a person's scheduled and webhook runs pause
  too ([below](#what-96-built)).
- **The main agent's turn.** `drive_opened` (and `drive_opened_from`, the
  same with a resume) returns `AgentReply { status: suspended, answer: None,
  suspension: Some(view) }` when the turn paused. The output filter runs only
  on a terminal answer. The pause is audited as `run_suspended` (`{session_id,
  turn_id, request_id, kind, tool, child_turn, expires_at}`) on the running
  principal with its chain.
- **Nested propagation.** A tool inside a sub-agent run pauses the child turn
  as usual. `forward_request` sees the child's row once its drive returns,
  records the dispatch's `run_context` on it (`{route, route_binds}`, migration
  `0077_agent_builder.sql`: the bound values exist nowhere else) and answers with a suspend
  envelope whose `child` names the child turn and its `expires_at`. The
  parent's driver checks that the child is a paused run of *this* turn
  (`parent_turn_id`), then pauses the parent with the child's kind, message and
  deadline and `child_turn` set. Every level up to the conversation's turn
  does the same, so the conversation's turn mirrors the innermost request.
  Each row has its own `request_id` (the column is unique); the one a client
  sees and answers is the conversation's.
- **One decision, innermost first** (`agents::resume`). `claim` walks
  `child_turn` from the conversation's turn down (at most `RunChain`'s
  `MAX_DEPTH` levels, each level's session must name the level above as its
  parent), checks `request_id` against the conversation's row, the decision
  against the innermost kind's options and the answerer against
  `answered_by()`, then claims the innermost row (the race two answers run) and
  every ancestor. `run_claimed` rebuilds each level from its session (owner,
  pinned version; a test conversation's draft) and its parent's pause (the
  chain is `RunChain::root` plus one `enter` per level, `CallSite` = the
  parent's turn and waiting call), resumes the innermost run with the
  decision, turns its outcome into the `forward_request` result exactly as a
  first dispatch would (`router::dispatch_result`, which also writes
  `sub_agent_finished`), and resumes each parent with that result
  (`ResumeFrom.child_result`: the waiting call is answered, not run again).
  A child that asks again pauses every level again on the new request. The
  resume is audited as `run_resumed` (`{session_id, turn_id, request_id, kind,
  decision, answered_by, waiting_turn}`, with `actor_id` for staff). A level
  that cannot be rebuilt (agent disabled since) errors every claimed turn.
- **The visitor's language.** A resumed run speaks the language the
  conversation's turn was asked in, whoever gives the decision: staff answer
  from the inbox in their own, and the timeout sweeper has none. Every new
  turn (`drive_opened`, so `run_turn`, the embed and A2A runners and a
  queued message alike) records `OpenedTurn.lang` as `chat_sessions.lang`
  (migration `0077_agent_builder.sql`) when the caller sets one: the public endpoint and an A2A
  task from the request, `run_turn` from `AgentTurn.lang`. That column is the
  only source of the run's language: the output filter's fallback texts, a
  timed-out handoff's `agent-human-no-answer`, the `lang` a handoff records,
  a verifier's and the A2A client's prompts all read it
  (`ToolContext::conversation_lang`, through the chain's root conversation, so
  a routed sub-agent speaks its caller's). A resume passes none, and a
  conversation that recorded none is English.
- **Secure values.** A `value` goes to the requesting tool through
  `ToolContext.suspend = Decided(Decision::Value)` and nowhere else.
  `Decision`'s `Debug` prints `<redacted>`; the audit rows carry the decision's
  shape only; for a `secure_input` the driver replaces every repetition of the
  value in the tool's result with `[secure input withheld]` before the result
  is stored or reaches the model, so a careless tool cannot leak it either. The
  stored turn holds the tool's (scrubbed) result as the waiting call's output.
  *For #95:* an MCP-backed `check_code` passes the code as an MCP argument, and
  `mcp_tool_audit` records arguments — the verifier must keep it out of there.
- **Visitor route** — `POST /api/v0/embed/resume` `{request_id, decision,
  value?}` (unknown fields refused), visitor token, admitted like a message
  (#92's rates and budget). `202 {turn_id}`; the rest arrives on the event
  stream. Only `secure_input` (`Answerer::Participant`): an approval answers
  `403 decision_for_staff`. Also `409 not_suspended` (nothing waiting, or a
  stale `request_id`), `400 decision_not_offered`, `409 turn_in_progress`.
  The refusals a visitor can act on are Fluent strings
  (`agent-embed-decision-for-staff`, `agent-embed-not-waiting`).
- **Staff route** — `POST
  /api/v0/agents/{id}/conversations/{session}/turns/{turn}/resume`
  `{decision, value?, request_id?}`, `can_manage_agents` plus a `write` share
  (or admin). Answers approvals and human answers in any of the agent's
  conversations; a `secure_input` only in a test conversation (version 0),
  where the manager plays the visitor (`403 decision_for_visitor` otherwise).
  Synchronous like the test chat: `200 {session_id, turn_id, status, answer,
  error, suspension}` once the resumed turn ended or paused again. It holds
  the conversation's claim meanwhile, so the visitor's stream
  shows the turn running. This is the backend #96's inbox calls.
- **What a visitor sees.** A suspended turn keeps `suspension` in the
  snapshot and `GET /api/v0/embed/session`, as
  `SuspensionView::for_participant`: `{request_id, kind, message?, options,
  expires_at}`, no `tool` and no `tool_call_id`; `options` is empty for a
  request staff answer. The event stream ends with a `suspended` frame of the
  same shape (instead of `idle`) when the conversation waits, or when the
  running turn pauses.
- **Messages behind a decision** (#96's decision: queued, not cancelling).
  `POST /api/v0/embed/messages` into a waiting conversation stores the user
  turn and answers `202 {turn_id: null, user_turn_id, placement: "queued"}`
  (`"started"` otherwise); a second one is `409 turn_in_progress`. The snapshot
  lists it in `waiting_turn_ids`. Once the resumed turn is terminal,
  `run_claimed` runs it as the next turn. The synchronous entry points
  (`run_turn`, the test chat) refuse instead: `AgentRunError::DecisionPending`,
  `409 decision_pending`.
- **Expiry.** `run_sessions::expired_run_suspensions` lists the conversations' own
  rows (principal-owned, no `parent_turn_id`) past their deadline;
  `agents::resume::resume_expired`, called by the existing 30-second sweeper,
  resumes each with its stored fallback, answered by `timeout`, skipping a
  conversation whose claim is held.
- **Restart.** Nothing waits in memory: every level is rebuilt from the rows
  on whichever process gets the decision.
- **Retention.** The #92 sweep keeps a conversation with a pending suspension,
  however idle; it goes on a later sweep once the decision or the expiry
  settled it.
- **Test chat.** `test-turn` answers carry `suspension` (the full view, tool
  included) and `status: suspended`; the SPA shows what the turn waits for
  with a value field or approve/deny, and answers through the staff route.
  A hand-off to a person also carries `suspension.context`: the handoff the
  pause stored in its `run_context` (`visitor_message`, `slots`, `inbox`, …),
  the same the Inbox shows staff. A test-chat pause never reaches the Inbox
  (`inbox::staff_item` skips the draft version); the manager answers it in
  the test chat.
- **Tests.** `agents/run/tests/suspend.rs` (secure input to the tool and
  nowhere in the database; a paused sub-agent pausing its caller and one
  staff decision resuming both; an expired approval denied; restart
  survival; a refused second message) and `tests/it/embed/suspend.rs` (the
  HTTP surface, with every table and every log line checked for the code).
- **For #95/#96.** A verifier returns `tool_suspend(SuspendRequest::
  secure_input(…))` and checks `Decided(Value)`; the widget renders the
  `suspended` frame's field and posts `/api/v0/embed/resume`. `permission:
  always_ask` wraps the tool in `AskFirst`; the inbox lists suspended agent
  turns and answers through the same resume as the staff route. Built in
  [#96](#what-96-built).

### What #96 built

Human in the loop on top of the agent-run suspend: per-tool approval, a
handoff to a person, an inbox where both are answered, notifications when a
turn starts waiting, and a resume path for a person's scheduled and webhook
runs. Migration `0077_agent_builder.sql`.

- **Per-tool approval** (`agents::approval`). `tool_resources.<tool>.permission`
  decides whether a call pauses for staff: `always_ask` wraps the tool in
  `AskFirst`, `always_allow` runs it as granted. Without a `permission` a tool
  asks first exactly when it is known to change something: the new
  `Tool::changes_state()` is `true` only for an MCP tool its server marks
  destructive and not read-only (built-in tools say nothing, so they are
  `false`). `tool_resources.<tool>.approval_timeout` (a duration, default
  `1h`) is how long the approval may take; an approval nobody gives is a deny
  (#96's decision; `SuspensionKind::timeout_fallback` enforces it). The gate
  sits outside the bound arguments, so an approved call still gets the
  gateway's values; a withheld tool is refused without asking anyone.
- **Handoff** (`agents::human`). A route with `human` is a target like a
  sub-agent. Keys: `notify` (`push`, `slack`, `discord`; absent = every
  channel), `inbox` (a label the inbox shows), `timeout` (default `30m`),
  `transcript` (`false` by default). Two ways in:
  - `request_human(question)`, a synthetic tool offered to the main agent
    whenever the spec has a `human` route. It needs an open human route
    (`router.order` first, else name order) and answers `no_open_route` with
    what is missing otherwise. *Chosen:* the argument is the question for
    staff, not a free-form reason, and it takes no other key.
  - `forward_request` picking a human route (it answered `human_unavailable`
    before). The route's `description` is the question, else the visitor's
    last message.

  Either pauses the call as `human_answer` with the question as `message` and
  a handoff stored in the pause's `run_context` (`{handoff: {route, question,
  visitor_message, slots, lang, inbox, notify, transcript?}}`).
  `SuspendRequest` gained `context` for this. `slots` is the model's view
  (a value only where the model wrote it, `set_by` otherwise), so a
  verifier's value never reaches the inbox; the transcript (the last 20 turns,
  each cut to 2000 characters) goes along only with `transcript: true`. The
  handoff is audited as `human_handoff` with the run chain (#100's analytics
  count that kind).
- **The answer goes through the main agent.** *Chosen over verbatim:* the
  staff answer is the waiting call's result (`{answered: true, answer,
  note}`) and the model passes it on in the visitor's language. The visitor
  and the staff member need not share a language, the conversation stays one
  the model continues, and the answer still passes the output filter (#89; a
  staff answer is trusted text, so identifiers staff quote pass). A staff
  `deny` is a tool error the model explains.
- **Nobody answers.** When a handoff's deadline passes, `run_claimed` does not
  ask the model: the waiting call is settled as unanswered and the turn ends
  with `agent-human-no-answer` in the conversation's recorded language (the
  visitor's `Accept-Language` on the public endpoint). A message queued behind it runs afterwards as usual.
- **Responders** (`agent_responders`, `db::agent_responders`). Users or
  groups who answer an agent's approvals and handoffs without a share — the
  support staff of §2. They need no `can_manage_agents`. Managed with a share
  like the rest of the agent; adding and removing one is audited
  (`responder_added`, `responder_removed`).
- **The inbox** (`agents::inbox`). An item is a conversation's own pause
  (`run_sessions::pending_suspensions`; a sub-agent's pause shows through its
  conversation's, with the innermost call's tool and arguments for an
  approval):
  - an agent conversation's `approval` or `human_answer`, outside the test
    chat (`agent_version = 0` stays in the test chat), for an admin, a manager
    with a `write` share, or a responder (`standing: manager | responder`);
  - a person's own paused conversation — a scheduled or webhook run — for its
    owner only (`standing: owner`).

  A responder gets the item and its minimal context: question, handoff
  context, an approval's tool and arguments, agent display name. Every other
  agent route refuses them (`403`, no agent-management permission), and so
  does the staff resume route.
- **Notifications.** Once per pause (`chat_turn_suspensions.notified_at`,
  set with `WHERE notified_at IS NULL`; a new pause is a new row), off the
  turn's path (`inbox::announce_in_background`), when an agent conversation's
  turn pauses (`drive_opened_from`, so a resumed turn that pauses again
  notifies again) or a person's headless run does:
  - **Web Push** to everyone who may answer: users holding a `write` share
    directly or through a group, and responders (admins without a share are
    not notified — they may answer everything and would be told everything),
    or the run's owner. Title and body from the catalog in each
    subscription's language; the link is `/inbox?item=<request_id>`.
  - **Slack and Discord** incoming webhooks (`agent_notify_channels`,
    `db::agent_channels`, `aiplane_agents::notify_channels`). The
    URL is the credential: sealed at rest (and in the reseal pass), never
    returned by the API, never in a log line or an audit row; only its host
    is kept in clear. A URL must be `https` on `hooks.slack.com` or
    `discord.com`/`discordapp.com` `/api/webhooks/…` (loopback `http` only,
    for tests). A message holds the agent, the kind and the absolute inbox
    link (`public_url`); with the channel's `details` on, also the question or
    the tool name (cut to 300 characters). No visitor message, transcript or
    slot value is ever sent. Slack text is escaped, Discord gets
    `allowed_mentions: {parse: []}`. A channel has its own catalog `lang`. A
    failed post is logged and skipped. Mail is not built.
- **Headless runs pause.** `headless::drive` makes every run suspendable,
  not only agent runs. A person's scheduled or webhook run that pauses is
  recorded as `waiting` (the SPA shows it as pending), notifies its owner and
  is answered from the inbox through the chat's own resume
  (`pages::chat::resume_turn`), so it continues as the owner's chat with the
  owner's tools; its expiry is the chat sweeper's. No built-in tool pauses a
  person's run yet; the path exists for the first that does (an MCP tool in
  `ask` mode is the obvious one).
- **API** (`aiplane-api::pages::json_inbox`). The inbox routes need a session
  only; everyone may ask, most see nothing.

  | Method | Path | Who | Purpose |
  |---|---|---|---|
  | GET | `/api/v0/agents/inbox` | session | `{items, count}`: each `{id (request_id), kind, standing, agent?, session_id, turn_id, title?, question?, call?: {name, arguments}, context?, options, created_at, expires_at}` |
  | POST | `/api/v0/agents/inbox/{id}/answer` | who may answer it | `{decision, value?}`; `202 {turn_id}`, the turn runs in the background and the visitor gets it on their stream. `404 inbox_item_not_found` for anyone else, `409 not_suspended` / `turn_in_progress`, `400 decision_not_offered` |
  | GET | `/api/v0/agents/inbox/events` | session | SSE: `inbox {count}` on attach and whenever the set changes (checked every 3 s), keep-alive comments, ends after 10 minutes for `EventSource` to reconnect |
  | GET/POST | `/api/v0/agents/{id}/responders` | read / write share | list; add `{subject_kind: user\|group, subject_id}` (`201`, `200` if already one, `404` for an unknown user or group) |
  | POST | `/api/v0/agents/{id}/responders/revoke` | write share | `{subject_kind, subject_id}`; `204` |
  | GET/POST | `/api/v0/agents/{id}/channels` | read / write share | list without URL; add `{kind: slack\|discord, name, url, details?, lang?}`; `422 invalid_webhook_url`, `409 channel_name_taken` |
  | DELETE | `/api/v0/agents/{id}/channels/{channel_id}` | write share | `204` |

  The inbox answer and the staff route both end in `agents::resume::claim` /
  `run_claimed` with `ResumedBy::Staff`, so `run_resumed` names the answerer
  as `actor_id`.
- **Widget and SPA.** The widget shows a waiting notice for a `suspended`
  frame with empty `options` and re-attaches every 10 s until the answer
  arrives ([`embed.md`](embed.md#when-the-agent-asks-a-person)); the SPA has
  `/inbox` with a live sidebar badge and the Responders and Notification
  channels cards in the workbench's Sharing tab ([`ui.md`](ui.md#inbox)).
- **Tests.** `agents/run/tests/hil.rs`: an `always_ask` tool pauses, staff
  approve and it runs, staff deny and it is a tool error, nobody answers and
  it is denied; `always_allow`; `request_human` to a responder whose answer
  reaches the visitor through the model, with the handoff's context and
  audit; the unanswered handoff in German with no model call; a human route
  through `forward_request`; one Slack post per pause, only on the channels
  the route names; a person's paused run in their own inbox only.
  `tests/it/embed/hil.rs`: the whole path over HTTP (visitor waits, responder
  answers from the inbox, visitor receives), what a responder cannot reach,
  who cannot answer, responder and channel management (no URL ever shown),
  and a scheduled run that paused, resumed from its owner's inbox.
- **Not built.** Answering in a Slack or Discord thread (inbox only, as the
  issue left open), mail, and an approval card in a person's interactive chat
  (its pauses are in the inbox, but the chat page does not render `suspended`
  yet).

## 4. Gates and validation

### Chosen: a JSON condition tree, evaluated by a small hand-written evaluator

A string language would need a parser, error recovery, quoting rules and a way
to show its errors in a form-based builder. A JSON tree needs none of that. The
UI composes it from dropdowns, and validation is a type check, not a parse.
Rejected alternatives:
- Pulling in an expression crate (CEL and similar) would add a dependency
  (`docs/dependencies.md`) for roughly 150 lines of evaluator.
- A crate would also bring operators we would then have to prove harmless.

```text
Cond := { all: [Cond] } | { any: [Cond] } | { not: Cond }
      | { slot, set: bool }
      | { slot, eq: Value } | { slot, in: [Value] }
      | { slot, provenance: "llm" | "verifier:<id>" | "host" }
      | { slot, max_age: Duration }          -- now - set_at <= d
```

A single leaf may combine several checks, e.g. `{slot, provenance, max_age}`.
They are ANDed together.

**Guarantees**
- **Total:** evaluation never fails. A missing slot makes every leaf on it
  false.
- **Deterministic:** the only input besides state is `now`, which is injected
  so tests fix it.
- **Explainable:** `eval` returns the failing leaves, which is exactly what
  `forward_request` reports back to the main agent.
- **Type-checked:**
  - Validation rejects unknown slots, an `eq` value of the wrong type, and a
    `provenance` the slot's `set_by` cannot produce.
  - Authorization-relevant routes are routes whose `agent` reaches a tool with
    a `bind`. Their `when` must contain a `provenance` leaf excluding `llm`. A
    gate that only checks model-written slots cannot guard a subject-bound call.

**Optional LLM classifier per route.** It may add a denial ("off-topic"). It
runs after the code gate passes and can only close the route.

### What #86 built

`aiplane-runtime::agents::gate`.

- **Grammar as implemented** — exactly the tree above. A leaf is
  `{slot, set?, eq?, in?, provenance?, max_age?}` with at least one check, and
  its checks are ANDed. `eq` and `in` compare the slot's whole JSON value
  (a `subject` compares as an object). `max_age` holds while
  `now - set_at <= d`; a value stamped after `now` is not old. `Cond::parse`
  reads a tree; the spec validator still reports shape problems with paths.
- **Semantics.** A slot that is missing, `invalid` (§3) or not declared fails
  every check on it, except a leaf that is only `set: false`, which holds.
  `all` holds if every child does, `any` if one does, `not` if its child does
  not. So `{not: …}` over a missing slot holds, as "missing = false" implies.
- **`evaluate(cond, GateInput { schema, state, now }) -> Vec<Unmet>`** returns
  every unmet leaf in document order; empty means open. For an `any` with no
  holding child it returns every child's leaves. An `Unmet` is
  `{path, slot?, kind, …, message}` with `kind` one of `missing`, `invalid`,
  `must_be_unset`, `not_equal {expected}`, `not_in {expected}`,
  `wrong_provenance {required, actual}`, `too_old {max_age}`, `excluded` (a
  `not` whose child holds), `unknown_route`, `denied`. The `message` tells the
  model what to do: `call set_email`, or `it is set by verifier:otp or host,
  not by you`. It never contains a value the model did not write; `expected`
  comes from the spec.
- **`RouteGates::from_spec(spec)`** holds each route's gate.
  `gate_status(route, input)` is `Open` or `Closed { missing }`; an unknown
  route is closed with `unknown_route`. A route is invoked through an
  `OpenRoute`, which only `open` and `open_reviewed` construct and only for an
  open gate, so dispatch (#88) cannot be reached around a closed one.
- **Classifier seam.** `DenyClassifier::review(route, &[SlotView]) ->
  Result<Verdict, String>` gets the model's view of the state, never trusted
  values. `open_reviewed` consults it only after the code gate opened.
  `Verdict::Deny` and an `Err` both close the route (`denied`); nothing it
  returns can open a closed gate. There is no LLM implementation yet, only a
  test double. *Decided, deferred:* the implementation will classify with the
  agent's main model unless the spec names a model for it.
- **Spec type checks.** On save and publish, each leaf must be able to hold:
  every `eq` and `in` value must pass the slot's own validator, and
  `provenance` must be in the slot's `set_by`. A route with a `bind` must have a
  gate that `requires_trusted_provenance`: a non-`llm` `provenance` leaf on
  every way through it (any child of an `all`, every child of an `any`, never
  under a `not`).
- **Deviations.**
  - The subject-bound check covers a route's own `bind`. Whether the routed
    sub-agent reaches a tool with a `bind` needs the sub-agent graph, so that
    half of the rule landed with #88 ([§3](#what-8788-built)).
  - No slot-to-slot comparison (`{slot: a, eq_slot: b}`): §4 has none and no
    route needs one yet.
- **Wired by #87.** `forward_request` calls `RouteGates`, returns
  `Closed.missing` to the model and writes a `route_decision` row
  ([§3](#what-8788-built)). There is no `gate` SSE event yet, and nothing
  calls `open_reviewed`: no spec key selects a `DenyClassifier` yet.

### Value validation without a JSON-Schema crate

Slots and `finish` schemas are checked by a minimal validator for a fixed subset:
- `type` (`string|integer|number|boolean|array|object`, plus `email` for slots)
- `enum`, `required`, `properties`, `items`
- `minLength`/`maxLength`, `minimum`/`maximum`, `pattern` through the existing
  `regex` dependency

An unsupported keyword is a **validation error on save**, never silently
ignored, so a schema can never under-validate. That is the reason not to accept
arbitrary JSON Schema.

## 5. Visitor sessions and embedding

```sql
CREATE TABLE agent_embed_keys (
    id           TEXT PRIMARY KEY NOT NULL,
    principal_id TEXT NOT NULL REFERENCES agents(principal_id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    key          TEXT NOT NULL UNIQUE,      -- "gwe_…", public by design (it ships in page source)
    origins      TEXT NOT NULL,             -- JSON array, exact scheme://host[:port]
    created_at   TEXT NOT NULL,
    revoked_at   TEXT
) STRICT;

CREATE TABLE visitor_sessions (
    id           TEXT PRIMARY KEY NOT NULL,
    principal_id TEXT NOT NULL REFERENCES agents(principal_id) ON DELETE CASCADE,
    embed_key_id TEXT NOT NULL REFERENCES agent_embed_keys(id) ON DELETE CASCADE,
    token_hash   TEXT NOT NULL UNIQUE,      -- SHA-256 of the visitor token
    session_id   TEXT NOT NULL,             -- the chat_sessions row of this conversation
    client_ip    TEXT,
    created_at   TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    expires_at   TEXT NOT NULL              -- last_seen_at + idle_ttl, slid on every request
) STRICT;
```

### Chosen: a script widget with a token in `sessionStorage`, not cookies

Decided requirements: the conversation survives a reload, does not span
sessions, and expires after an idle TTL that is configurable (default 30 min,
sliding).

| Option | Verdict |
|---|---|
| Gateway cookie in an iframe | Third-party cookies are blocked or partitioned in Safari and Chrome. We would also need CSRF defences on a cross-site cookie. Rejected. |
| CHIPS (`Partitioned` cookie) | Works in Chromium and Firefox, but Safari's support and ITP behaviour are the weak link. It still needs `SameSite=None` and CSRF handling. Rejected. |
| **Script in the host page (shadow DOM), visitor token in `sessionStorage`, sent as `Authorization: Bearer`** | Storage is first-party to the host page. `sessionStorage` survives a reload and dies with the tab, which is exactly "reload-safe, not across sessions". No cookie means no CSRF. **Chosen.** |

**Starting a session.** `POST /api/v0/embed/sessions { key }`
- The gateway checks the key, the request's `Origin` against `origins`, and the
  agent being live.
- It mints a visitor token (`gwv_…`, stored hashed) and returns it.

**Using a session.**
- The widget sends messages with `POST /api/v0/embed/messages`.
- It reads the `chat_json` events through `fetch` streaming, not
  `EventSource`, because `EventSource` cannot send an `Authorization` header.
- Every request slides `expires_at`. An expired token gets `401
  visitor_session_expired`, and the widget starts fresh.

**CORS.** Only the `/api/v0/embed/*` routes get it, scoped to the key's origins.
Every other `/api/v0` route keeps refusing cross-origin requests.

**What the origin check is for.** `Origin` can be forged by any non-browser
client and the embed key is public. The origin check stops other websites from
embedding the agent, not abuse. The protection against abuse is:
- the per-visitor and per-IP rate limits and the owner's budget (#92),
- the agent's default-deny grants,
- the gates.

The doc says this plainly so nobody mistakes the key for a secret.

**Secure input** (verifier codes). The widget renders a dedicated field and
posts it to `POST /api/v0/embed/resume` with the `request_id` of the
`suspended` frame. That resolves a `secure_input` suspension (§3) directly. The
code never enters model context or the stored transcript
([built](#what-agent-run-suspend-built)).

**Output policy.** A public main agent uses `OutputPolicy::Buffered`. Each
answer is held until it passes the output filter (#89), then sent as one block,
while `status` events keep the widget alive. Token-by-token streaming and a
filter that sees the whole answer cannot both hold, and the filter wins for
untrusted audiences.

### What #91 built

- **Migration `0077_agent_builder.sql`** creates both tables, with three
  changes from the sketch above:
  - `agent_embed_keys.key` is **`key_hash`**, the SHA-256 of the `gwe_` key,
    like every other credential. The key is public anyway, but storing it in
    clear would make the table a list of working keys. The plaintext is in
    the create response only. The table also records `created_by`.
  - `visitor_sessions` gains `idle_ttl_secs` (the idle TTL the session started
    with, so sliding needs no spec parse) and `max_expires_at` (the absolute
    cap). `session_id` has a foreign key with `ON DELETE CASCADE`: a
    conversation removed by retention (#92) ends its visitor session.
  - `chat_sessions.visitor_id` is a plain `ADD COLUMN … REFERENCES
    visitor_sessions(id) ON DELETE SET NULL`. A new column with a `NULL`
    default may carry a foreign key, so no rebuild was needed and no row
    moves.
- **Rows** live in `aiplane-agents::db::{embed_keys, visitor_sessions}`.
  Creating and revoking a key writes `embed_key_created` /
  `embed_key_revoked` audit rows. `visitor_sessions::start` opens the
  principal-owned conversation (`agent_version` = the live version), the
  visitor row and the back link in one transaction (`create_principal_session`
  now takes any executor for that). `lookup` resolves a token without
  touching it; `slide` counts an accepted request. Both take `now`.
- **Lifecycle.**
  - *Start:* `POST /api/v0/embed/sessions {key}`. The key must hash to a live
    row, the request's `Origin` must be in that key's `origins`, and the agent
    must be enabled and published. The answer is `201 {token, expires_at,
    idle_ttl_secs, agent: {display}}` with a fresh `gwv_` token.
  - *Every later request* sends `Authorization: Bearer gwv_…` and re-checks
    the chain: session not expired, key not revoked, `Origin` allowed (see
    below), agent enabled and published. Only then does the session slide, to
    `min(now + idle_ttl, max_expires_at)`. So a revoked key or a disabled
    agent ends open conversations at their next request.
  - *Idle TTL:* the starting version's `publish.idle_ttl`, default 30 min. *Absolute
    cap:* 24 h (`MAX_VISITOR_SESSION`); `sessionStorage` normally ends the
    session sooner, with the tab.
  - *Reload:* the widget finds its token in `sessionStorage` and calls `GET
    /api/v0/embed/session` for the transcript, or `…/events`. After the TTL
    either answers `401 visitor_session_expired`, and the widget starts a new
    session.
- **Routes** (`aiplane-api::pages::embed`; the key routes in `pages::json_embed_keys`; routed in `gateway`):

  | Method | Path | Purpose |
  |---|---|---|
  | POST | `/api/v0/embed/sessions` | Start a visitor conversation `{key}` |
  | GET | `/api/v0/embed/session` | The conversation: `{expires_at, idle_ttl_secs, agent, live_turn_id, turns}` |
  | POST | `/api/v0/embed/messages` | `{text}` (≤ 8000 characters, no other field); `202 {turn_id, user_turn_id, placement}` (`queued`, with `turn_id: null`, behind a pending decision) |
  | POST | `/api/v0/embed/resume` | `{request_id, decision, value?}`: answer a `secure_input` ([§3](#what-agent-run-suspend-built)); `202 {turn_id}` |
  | GET | `/api/v0/embed/events` | `chat_json` frames for `fetch` streaming |
  | GET | `/api/v0/agents/{id}/embed-keys` | Keys of the agent (`read` share); never the key |
  | POST | `/api/v0/agents/{id}/embed-keys` | `{name, origins}` (`write` share); `201 {embed_key, key}` |
  | POST | `/api/v0/agents/{id}/embed-keys/{key_id}/revoke` | `write` share; `204` |

  Errors, all in the `/api/v0` envelope: `401 embed_key_invalid`, `403
  embed_key_revoked`, `403 origin_not_allowed` (names the origin to add), `403
  agent_disabled`, `409 agent_not_published`, `401 visitor_session_invalid`,
  `401 visitor_session_expired`, `409 turn_in_progress`, `503
  agent_runtime_unavailable`.
- **What a visitor sees.** Turns are filtered before they leave: no tool
  calls, no reasoning, no model name, no steers or suspension, no content of
  an unfinished answer, and an errored turn carries a generic message instead
  of the upstream's. The token names the conversation; no request carries a
  session id (`messages` refuses unknown fields).
- **Event stream, buffered.** `snapshot` first, with `live_turn_id` when a
  turn runs. Without one, `idle` and the stream ends. With one, the stream
  subscribes to the conversation's worker in the session worker registry and
  reads that one turn (never the conversation) once no worker holds it
  (`agents::embed::released`, paced by `pages::turn_wait`). It listens for
  `TurnUpdate::Released` only: ticks and `Finalized` come while the answer
  may still be unfiltered, so a visitor never sees partial text, tool
  internals or an answer the output filter has not ruled on. No polling: the
  claim is an RAII guard, so its release always comes. And — the A2A
  task streams alike — once it is terminal, sends its whole
  answer as one `turn_delta` with `full: true` and then `turn_finalized`.
  *Deviation:* the "`status` events" above are SSE comment lines (`:
  working`) every 15 s. There is no status to report beyond "still running",
  and a comment needs no new `chat_json` event. A stream gives up after 10
  minutes with `idle`; the widget re-attaches.
- **CORS.** `EmbedCorsLayer` (`aiplane::rama_server::embed_cors`) handles
  `/api/v0/embed/*` only. A preflight carries neither key nor token, so the
  layer reflects an `Origin` only if some live key of an enabled agent lists
  it (and answers a preflight from any other origin `403`, without CORS
  headers). The handler then checks the origin against the request's own key.
  The set of such origins is cached in memory (`embed_keys::EmbeddableOrigins`),
  not read per request: creating or revoking a key and disabling or deleting
  an agent clear it at once, and a 30 s TTL catches writes from outside the
  process. No `Allow-Credentials`, `Max-Age` 600 s so a revoked origin stops working
  quickly. Every other `/api/v0` route still gets no CORS headers.
- **The runner.** The endpoint opens the turn rows (the visitor's user turn
  and an `in_progress` assistant turn), then hands an `OpenedTurn {agent_id,
  version, session_id, turn_id, visitor_id}` to
  `aiplane_runtime::agents::embed::AgentTurnRunner::run(state, turn)` in a
  background task. `OpenedTurn` is the same type `agents::run` uses: its
  `run_turn` opens the rows itself and then calls the same `drive_opened` the
  production runner `LiveAgentRunner` calls, so the two paths share one
  driver. `main.rs` installs `LiveAgentRunner` with
  `RamaState::with_agent_runner`; a profile that cannot load (agent disabled,
  no healthy model) errors the turn with that reason. Without a runner (only
  in tests) `messages` answers `503 agent_runtime_unavailable` and stores
  nothing. A runner that leaves the turn unfinished, or panics, has its turn
  errored by the endpoint (`spawn_guarded` settles it before the claim drops),
  and one claim per conversation (`agents::embed::claim`, a worker in
  `RamaState::chats`) keeps two messages from running at once.
- **A conversation is pinned to its version.** It runs the version that was
  live when it started, recorded in `chat_sessions.agent_version`; publishing
  or rolling back changes only conversations started afterwards. Versions are
  immutable and deleted only with the agent, so a pinned version always
  exists. `agents::run::run_turn` applies the same rule to a continued
  session (`RunProfile::load_version`).
- **Origins: the key's, narrowed by the spec's.** A request's `Origin` must be
  in the embed key's `origins` and, when the conversation's version sets
  `publish.origins`, in those too. The refusal says which list lacks it. The
  CORS layer only knows the keys, so an origin the spec excludes still gets a
  preflight answer, and the handler then refuses it.
- **Rate limits** were a stub here (`pages::embed::admit`, always admitting);
  #92 replaced it ([below](#what-92-built)).
- **Not built here.** Resuming a suspended visitor turn, which came later
  ([What agent-run suspend built](#what-agent-run-suspend-built)): until then
  the visitor view dropped `suspension`, and a suspended turn ended the event
  stream with `idle`. The output filter itself is #89. The widget is #94 (below). User-visible text is in
  the error envelope's English `message` with a stable `code`; the widget is
  expected to show its own Fluent strings per `code`.

### What #94 built

- **`web/embed/`**, a standalone bundle (own Vite config, no SvelteKit),
  built by `build-web` to `target/frontend/build/embed.js` and served by the
  gateway at `/embed.js` from `AIPLANE_STATIC_DIR`. About 74 kB, 17 kB
  gzipped, including daisyUI's CSS for the used components and all six
  languages. Owner documentation: [`embed.md`](embed.md).
- **Shadow DOM, daisyUI inside it.** Tailwind v4 + daisyUI v5 are compiled for
  the widget only and adopted as a constructed stylesheet (not subject to the
  host's `style-src`). `forShadowRoot` rewrites `:root` to `:host` and turns
  `@property` defaults into declarations. Theming is daisyUI's custom
  properties on the `croit-aiplane-embed` element plus `data-theme`.
- **Session lifecycle** is `TokenStore` + `EmbedApi` (`web/embed/api.ts`): a
  token in `sessionStorage` (accessor and calls guarded, memory fallback),
  resume on load, `visitor_session_expired` starts a fresh session and resends
  the message once, `Authorization` header, `credentials: 'omit'`.
- **Safe rendering.** Answers are parsed by a small markdown subset
  (`markdown.ts`) into a tree and built with `createElement`/`textContent`;
  there is no `innerHTML`. The SPA's `marked` + DOMPurify were left out to
  keep the bundle small.
- **Strings** are the `embed-*` Fluent keys. `gen-locales` also writes
  `web/embed/locales.generated.ts`, checked by
  `i18n_drift::the_embed_catalog_matches_the_fluent_sources`.
- **Not built here:** the secure-input field, which #95 added
  ([below](#what-95-built)). The waiting view for a request staff answer came
  with [#96](#what-96-built).
- **`dev-ui`** now installs `LiveAgentRunner` and seeds a published agent on
  `demo-model` with a fixed embed key for `http://localhost:8000`.

### What #92 built

Limits that make an embedded agent safe to leave running, an owner budget, the
model rule, and retention.

- **Migration `0077_agent_builder.sql`.** `usage_events` gains `agent_id` (the
  main agent at the root of the run's call chain, `NULL` outside a run) and
  `chain` (the serialized `RunChain`), with an index on `(agent_id,
  created_at)`; `visitor_sessions` gains an index on `(principal_id,
  client_ip)`. `UsageRecord::in_run(chain)` fills both. `user_id` still names
  the principal that made the call, so a sub-agent's call reads
  `user_id = <sub-agent>, agent_id = <main agent>`.
- **Every model call of a conversation is metered**: the main agent's rounds
  and the sub-agents' (as before, now with `agent_id`), and the router's
  classifier call, which wrote no usage row until now. Usage rows need
  `[usage] enabled`; with metrics off, nothing is ever spent against a budget.
- **Spec settings** (`publish`, validated on save):
  - `rate_limits.visitor` / `rate_limits.ip`: `{max, per}`, both required,
    `max ≥ 1`, `per` a duration. Defaults when unset: **20 messages per 10
    minutes per visitor session**, **60 events per 10 minutes per client IP**.
  - `budget`: `monthly_cost` (> 0, in the currency models are priced in) and/or
    `monthly_tokens` (≥ 1). No default.
  - `retention_days` (existing key): default **30**.

  Limits, budget and retention are read from the agent's **live** version, not
  the version a conversation is pinned to: lowering a budget or a retention
  period applies to every open conversation at once.
- **Visitor rates** (`aiplane_agents::rates::admit_visitor`, `rates::Rate`).
  An exact sliding window, not the hour-snapped `Window` of spend limits: a
  visitor told to wait 40 s may send after 40 s. What is counted is the
  **admission**: every request the gate lets through is one event, recorded
  by the one rate primitive, `rates::record_within` — the windows are read
  and the event written in one `WriteTx` (`BEGIN IMMEDIATE`), so parallel
  requests queue on the write lock instead of all passing the check before
  any is counted (they did, when the count was the rows a request left
  behind). The per-visitor bucket counts the admissions into one
  conversation (`rates::Counter::conversation`, the embed visitor or the
  A2A context); the per-IP bucket counts every admission from that IP to the
  agent on every channel (`Counter::ip`), so opening a fresh conversation per
  message does not dodge the per-visitor limit. The events are rows of
  `rate_events` (`migrations/0077_agent_builder.sql`), one per window, each
  expiring a window after it was written. A refused request writes nothing,
  so it never counts; the owner budget is checked first for the same
  reason. Gated: `POST /api/v0/embed/sessions`, `…/messages` and `…/resume`;
  reads (`GET …/session`, `…/events`) cost the agent nothing and the widget
  re-attaches freely.
- **Owner budget** (`limits::Enforcer::check_agent`). The spec's
  `publish.budget` becomes month-window limits labelled `AgentSpec`; an
  operator may add a `limits` rule with the new subject **`system`** (subject
  id = the agent's id, set at `/api/v0/admin/limits`). Each is its own
  ceiling — the tightest decides, none widens another — measured against
  `usage_events.agent_id`. Debt model, like every other limit: the message
  that crosses the line is served, the next is refused. The owner's budget is
  part of the agent and applies even with `[limits] enabled = false`; that
  switch only governs the operator's rules. Both are checked on starting a
  conversation and on every message.
- **Refusals.**
  - Rate: `429 visitor_rate_limited` with `Retry-After` (when the oldest
    counted event leaves the window) and the Fluent message
    `agent-embed-rate-limited` in the request's `Accept-Language`.
  - Budget: `503 agent_unavailable` with `Retry-After` and
    `agent-embed-unavailable` ("temporarily unavailable"). The visitor is not
    told why.
  - Every refusal is an `agent_audit` row `limit_refused` on the agent:
    `{limit: visitor_rate|ip_rate, visitor_id, max, per_secs,
    retry_after_secs}` or `{limit: budget, set_by: agent|operator, dimension,
    window, max, used, retry_after_secs}`. The client IP is not stored in it.
    A flood is folded: one event per (agent, subject, limit) per minute with
    `count: 1` and a `window_id`, and — when more were refused in that
    minute — one more as it closes, whose `count` is the rest and whose
    `folds` names the window (the log is append-only since #111). At
    most 10 000 such windows are open at once; past that, a new subject's
    refusals go to the agent's overflow row for the limit (`visitor_id`
    null), so a storm from rotating IPs is still counted without growing
    memory.
  - Managers see the state in `GET /api/v0/agents/{id}` under `agent.limits`:
    `{rate_limits: {visitor, ip}, retention_days, budget: [{set_by,
    dimension, window, max, used, exceeded, refreshes_at}], available,
    unavailable_reason}`, where `unavailable_reason` is the budget detail
    above.
- **Models.** An agent run reaches only models that are both granted and
  named by its spec (or its gateway default):
  `PoolAccess::for_system_models(principal, listed)`. The narrowed access
  applies to the turn's rounds (`main.model`), the router's classifier
  (`router.model`), the topic guard (`scope.classifier_model`) and the
  conversation's compaction summary. A sub-agent uses its own spec's model.
  Tools that call a model themselves (image generation) keep the principal's
  model grants: the tool grant is what allows them. A grant names a model
  and records the pools the granting manager could use for it: a model name
  served by a self-hosted pool and by a cloud pool restricted to a group the
  manager is not in reaches only the self-hosted one, so a grant never takes
  an agent where its manager's own chat could not go ([Models](#models)).
- **Retention** (`agents::retention`, `db::agent_retention`). A sweeper runs
  at boot and every hour (`spawn_retention_sweeper`, started in `main.rs`).
  Per agent it deletes the conversations — root sessions it owns, visitor and
  test-chat (`agent_version = 0`) alike — whose last activity
  (`chat_sessions.updated_at`) is older than `retention_days`, together with
  every sub-agent run below them, however deep. The foreign keys take turns,
  tool calls, `agent_state` and the visitor session. The selection requires
  `user_id IS NULL` at every step, so a person's chat is never touched. Each
  sweep that deleted something writes `conversations_swept` with
  `{retention_days, conversations, sub_agent_runs}` — counts only.
- **Deviations and limits of this.**
  - The per-IP bucket uses the client IP the gateway derives (#98): the TCP
    peer, or — only when the peer is in `$AIPLANE_TRUSTED_PROXIES` — the
    rightmost `X-Forwarded-For` hop that is not itself a trusted proxy, the
    same resolution GeoIP uses. With nothing trusted a forged header changes
    nothing; behind a reverse proxy that is *not* listed, every visitor
    shares the proxy's bucket.
  - A refusal storm writes one audit row per refused request.
  - The compaction summary call is still not metered, for agents or people.
  - The admin limits page in the SPA does not offer subject `system` yet; the
    API accepts it.

### What #95 built

Three verifier kinds under `verifiers.<id>`, the only writers besides the
host of a slot the model cannot write. Code in
`aiplane-runtime::agents::verifier` (runtime) and `agents/spec/verifiers.rs`
(validation); rows in `aiplane-agents::db::agent_verifiers`
(migration `0077_agent_builder.sql`).

```yaml
verifiers:
  otp:                                   # one-time code through the agent's own connector
    kind: mcp_code
    connector: erp                       # an `agent`-scope connector granted to this agent
    send_tool: send_code                 # default; called with {email}
    check_tool: check_code               # default; called with {email, code}, answers {valid: true, …}
    email_slot: email                    # an `email` slot (the model may write it: it is the claim)
    writes: { verified: result, verified_email: input.email }
    max_attempts: 5                      # default 5, at most 10, per code
    code_ttl: 10m                        # default 10m, at most 1h
    send_limits:                         # sliding windows on sends; defaults shown
      email:   { max: 5,  per: 1h }      # per address, across every conversation
      ip:      { max: 20, per: 1h }      # per client IP
      session: { max: 3,  per: 15m }     # per conversation
    assurance: email_verified            # optional label, recorded with each outcome
  kyc:                                   # weaker: confirms what the visitor knows
    kind: lookup
    tool: mcp__erp__find_customer        # a granted tool or connector tool; answers {valid: true, …}
    inputs: { name: state.name, number: state.customer_number, region: { const: eu } }
    writes: { verified: result }
    max_attempts: 5                      # per conversation
    assurance: low                       # required
  site:                                  # the embedding website vouches; slots are written as `host`
    kind: host_jwt
    algorithm: HS256                     # or RS256 / ES256
    secret: "…"                          # HS256 only; sealed on save, stored as `secret_sealed`
    # public_key: "-----BEGIN PUBLIC KEY-----…"   or   jwks_url: https://www.example.com/jwks.json
    issuer: https://www.example.com
    audience: support-agent
    max_lifetime: 10m                    # default 10m: exp - iat may be at most this
    claims: { verified: { customer_id: sub, plan: plan } }   # slot → claim, or field → claim
```

- **Writes.** A source is `result` (the tool's whole answer without
  `valid`), `result.<field>`, or `input.<arg>` — never the code. Every write
  is resolved and checked against its slot first, and stored only if all fit,
  through `write_trusted` with `TrustedWriter::Verifier(<id>)` (or `Host`).
  A target slot must list `verifier:<id>` (or `host`) in `set_by`, which the
  validator checks; `set_by: verifier:<host_jwt id>` is refused with "list
  `host`".
- **`mcp_code` flow.** `verify_<id>_request_code()` reads the email slot,
  checks the three send windows (`rates::sliding_window`, the #92 rate
  type, with new scopes `email` and `session`) and records the send with
  the same primitive as a visitor admission (`rates::record_within` on
  `agent_verifiers::window` counters, one `WriteTx`, so parallel requests
  cannot all pass the check before one is counted; a lookup's attempts go
  through the same call, and its `attempts_left` is the count the attempt
  was admitted against, `Admitted::seen`), calls
  `send_code`, stores the outstanding code's address hash, send time and
  expiry, and pauses the turn with `SuspendRequest::secure_input` (message
  `agent-verifier-code-sent`, timeout `code_ttl`). The widget's secure field
  answers on `/api/v0/embed/resume`; the same call runs again with
  `Decided(Value)`, which checks expiry, that the email slot still hashes to
  the address the code went to, and takes one attempt in a single `UPDATE …
  WHERE attempts < max` (parallel guesses cannot share one), then calls
  `check_code`. A miss answers `wrong_code` with `attempts_left`; the last
  one answers `locked`. `verify_<id>_submit_code()` asks again for the same
  code (`agent-verifier-code-again`) without sending a new one. Both tools
  refuse every argument by name (`code`, `provenance`, …).
- **Enumeration.** The visitor and the model get the same pause and the same
  `wrong_code` for an unknown address: `send_code`'s answer (or error) is
  recorded as `delivery: accepted|refused` in the owner's audit only.
- **Where the code is not.** It reaches the connector's `check_code` and
  nothing else. The verifier's tools declare `sensitive_args`; the layer's
  `get_with_sensitive_args` makes an audited connector record the activity
  log's redaction marker (`{"redacted":true}`,
  `agent_audit::redaction::redacted_arguments`)
  for both the arguments and a failed call's error in `mcp_tool_audit`; the
  answer is passed through `Redaction::withhold` before a slot is written. rmcp
  dumps outgoing MCP requests at `trace`, so the binary's log filter
  (`aiplane::logging`) pins `rmcp::service=debug` whatever `RUST_LOG` asks.
  `tests/it/embed/verifiers.rs` greps every table and every log line (at
  `trace`, through that filter) for the code.
- **`lookup` flow.** `verify_<id>()` builds the tool's arguments from state
  (refusing while a slot is unset), counts lookups per conversation against
  `max_attempts`, and answers `not_confirmed` alike for no match and a
  partial one.
- **Host JWT.** `POST /api/v0/embed/identity {token}` (visitor token):
  `host_jwt::accept` checks the header's `alg` equals the configured one
  before anything else (no HMAC-with-a-public-key confusion), the signature,
  `exp`/`nbf`/`iss`/`aud` (30 s leeway, `exp`, `iat`, `iss`, `aud`
  required), `exp - iat ≤ max_lifetime`, and a `jti`, when present, once per
  agent (`agent_identity_jtis`, kept until `exp`). JWKS documents are cached
  five minutes per URL and refetched for an unknown `kid`. The JWKS URL is
  chosen by the agent's owner, so it is fetched through the same SSRF guard
  as an A2A route (`outbound_guard`, `Policy::agent`, [below](#what-101-built)):
  resolved and pinned, no redirects, at most 64 KiB, link-local always
  refused, and loopback, private addresses and plain `http` only under
  `$AIPLANE_ALLOW_PRIVATE_NETWORKS=true`. Answers: `200 {slots}`, `401
  identity_token_invalid` (the message says what is wrong, never a claim
  value), `409 identity_token_replayed`, `422 identity_not_configured`, `503
  identity_keys_unavailable`. The `503` message is generic — no URL, status
  code or parse error, so the endpoint is no probe into the gateway's
  network; the reason goes to the log and to the `host_identity` audit row
  (`jwks_url`, `error`), where the owner sees it. A refused token writes
  nothing. An accepted one is stored in one transaction: every mapped slot
  is checked first, then the `jti` is spent and all slots are written
  together (`state::write_trusted_all`), so a failed write leaves no slot
  behind and the `jti` unspent — the website can retry with the same token.
- **Secrets.** `POST /api/v0/agents` and `PUT …/draft` validate the plain
  `secret` (at least 32 characters) and then replace it with `secret_sealed`
  (the at-rest `Crypto`), so no draft, version, audit row or GET carries it.
  A GET → PUT round trip keeps `secret_sealed`.
- **Validation.** Shape on every save (keys per kind, grants, ranges,
  algorithms, PEM keys parse, JWKS URL scheme, write sources, slot types);
  what a verifier needs to run on publish (`connector`/`email_slot`/`writes`;
  `tool`/`inputs`/`writes`/`assurance`; `algorithm`, a key, `issuer`,
  `audience`, `claims`). One `host_jwt` per spec. An incomplete verifier in a
  draft offers no tool (`Verifiers::from_spec` skips it).
- **Audit.** `verifier_outcome` (`{verifier, kind, step, outcome,
  assurance, session_id, turn_id, …}`; outcomes `code_sent`, `verified`,
  `wrong_code`, `locked`, `expired`, `email_changed`, `rate_limited`,
  `not_confirmed`, `too_many_attempts`, `write_failed`) and `host_identity`
  (`{session_id, outcome, reason | slots}`, in the chain of the conversation
  the token was presented for, with its `conversation_id`). A refused token
  is counted like a refused visitor (`embed::RefusalAudit`): one event per
  conversation and reason when a window opens, one with the rest of the
  `count` when it closes, so a flood of bad tokens is two rows, not one a
  request. Never a code, an address or a claim value.
- **The system message after a resume.** The driver now rebuilds an agent's
  system message on the first round after a resume too, so a slot the
  resumed call just wrote (the verifier's) shows on the next request and
  the gate reads `open`.
- **Widget.** A `secure_input` pause renders a masked field
  (`type=password`, `autocomplete=one-time-code`, `embed-code-*` strings)
  under the transcript, answered with `decision: value` (or `deny` from
  Cancel) on `/embed/resume`; `data-identity-token` and
  `croit-aiplane-embed.setIdentityToken(token)` send the host token once per
  conversation ([`embed.md`](embed.md#signed-in-visitors)). The builder edits
  verifiers in the JSON panel (or, for the assistant's own `identity`, in the setup's identity step); its Settings section lists them with their
  issues.
- **Deviations.**
  - The design table put verifier tools in `aiplane-tools`; they need the
    run (state schema, `TrustedWriter`, the principal's connector layer), so
    they live in `aiplane-runtime` next to `set_<slot>`.
  - The check contract is fixed: `valid: true` in the tool's answer (a JSON
    object, structured or as text). A RAG search answers hits, not a
    verdict, so a `lookup` names a tool that answers that contract.
  - `assurance` is a free label; levels and their names are still open.
  - No magic-link variant.

### What #100 built

Analytics for a manager: what an agent did over a period, derived from rows
that already exist. No second event store.

- **Migration `0077_agent_builder.sql`**: one index,
  `agent_audit (principal_id, kind, created_at)`.
- **`GET /api/v0/agents/{id}/analytics?from=&to=&version=`** (`read` share,
  admins always; same 403/404 rules as the other agent routes). `from` and `to`
  are RFC 3339 instants or `YYYY-MM-DD` UTC days; a day as `to` includes that
  whole day. Default: the last 30 days. Longer than 366 days, `from >= to`, an
  unparseable bound or `version < 1` is a 400 that says what to change.
  `version` narrows to one published version.
- **Response** (counts and route, slot and reason names only; no visitor
  content, no session or visitor ids):
  `{from, to, version, currency, conversations, turns, sub_agents: {dispatched,
  finished, incomplete, incomplete_by_reason: {kind: n}}, gate_refusals:
  {total, by_route: {route: n}, by_missing_slot: [{route, slot, count}]},
  routes_chosen: {route: n}, output_blocks: {total, by_action}, limit_refusals:
  {total, by_kind}, human_handoffs, usage: {requests, prompt_tokens,
  completion_tokens, tokens, cost}, daily: [{day, conversations, turns, tokens,
  cost, refusals}]}`. `daily` has one UTC-day bucket for every day of the range,
  quiet ones as zeros.
- **Where each number comes from.**
  - `conversations`: root conversations (`parent_turn_id IS NULL`) the agent's
    principal owns, by `created_at`. `turns`: their `user` turns, by the turn's
    `created_at`. A conversation the retention sweeper deleted is gone from
    both.
  - `routes_chosen`: `route_decision` rows with a `picked` route.
    `gate_refusals`: `route_decision` rows with `reason: no_open_route` (no
    route was open for the request); each closed route counts once in `by_route`
    and each of its unmet slots once in `by_missing_slot`. A decision the
    classifier declined (`picked` null, any other reason) is not a gate refusal.
  - `sub_agents`: `sub_agent_dispatched` and `sub_agent_finished` rows;
    `incomplete_by_reason` is `outcome.reason.kind`.
  - `output_blocks`: `output_blocked`, by `action` (`redacted`, `withheld`).
  - `limit_refusals`: `limit_refused`, by `limit` (`visitor_rate`, `ip_rate`,
    `budget`).
  - `human_handoffs`: audit rows of kind `human_handoff`
    (`agent_analytics::HUMAN_HANDOFF_KIND`). #96 is not merged, so nothing
    writes that kind yet and the count is 0; it starts moving the day #96
    writes it, provided it records the agent's chain.
  - `usage`: `usage_events` with `agent_id` = the agent, which includes the
    sub-agents' calls and the classifier's. Tokens are `total_tokens`, or
    prompt + completion when that is missing. Needs `[usage] enabled`.
- **What is never counted.** Builder test conversations (`agent_version = 0`;
  their audit and usage rows carry the draft version in the chain's main
  frame), another agent's rows, and a run of this agent as somebody else's
  sub-agent (the chain's main frame names the other agent).
- **Version filter.** A row's version is the main frame of its serialized call
  chain (`frames[0].version`); conversations use `chat_sessions.agent_version`.
  `limit_refused` rows have no chain, because a refusal happens before any
  version runs, so they are left out while a version is selected (the SPA says
  so).
- **Aggregated in SQL.** Conversations, turns, usage and the audit kinds that
  are only counted are `GROUP BY substr(created_at, 1, 10)` (the UTC day)
  queries. The version filter reads the chain with `json_extract`; the exact
  range compares on `rtrim(created_at, 'Z')`, because RFC 3339 text with
  fractional seconds of varying length orders correctly only without its `Z`
  (`db::window_key`); whole-day bounds alongside keep the indexes in use. Only
  the audit rows whose `detail` carries the numbers (route decisions, sub-agent
  outcomes, output blocks, limit refusals) are fetched. A range is capped at
  366 days.
- **SPA.** An *Analytics* tab on `/agents/{id}`
  ([`ui.md`](ui.md#agent-builder)).
- **Tests.** `crates/aiplane/tests/it/agent_analytics.rs` seeds two agents,
  two versions, a test conversation, out-of-range rows and a visitor's text and
  id, and asserts every number exactly, the daily series, the version filter,
  no leakage between agents, no visitor content, and the share rules.

### What #99 built

Evaluation: stored test cases per agent, run against the draft or a published
version and judged on more than the final answer.

- **Migration `0077_agent_builder.sql`**: `agent_test_cases`, `agent_test_runs`,
  `agent_test_results`. Rows live
  in `aiplane-agents::db::agent_tests`; the logic in
  `aiplane-runtime::agents::eval` (and `eval_judge` for the rubric).
- **Case.** `{name, script, expect, rubric?}`; the name is unique per agent. A
  case is validated when stored (`422 invalid_test_case` with `issues[{path,
  message}]`): a case that checks nothing, or names an unknown key, is refused.
- **Script.** An ordered list of steps: `{"say": "<visitor message>"}` or
  `{"write": {"slot", "value", "writer"}}`, with `writer` either `host` or
  `verifier:<id>`. A write goes through `write_trusted` with a `TrustedWriter`
  built from that text, so the slot's `set_by` and its type still apply; a
  refused write fails the case with the reason. It is the only place text
  becomes a trusted writer, and only the test runner reaches it: no public or
  visitor path accepts a script. The model's own `llm` writes cannot be
  scripted. The first step must be a `say`, because the conversation only
  exists once the visitor has spoken. A turn that suspends (an approval, a
  secure input) before the last step stops the script: a script cannot answer
  it.
- **Expectations** (`expect`, all deterministic; an omitted key is not
  checked):

  | Key | Meaning |
  |---|---|
  | `gates.<route>` | `{open: bool, missing?: [slot]}`: the gate after the last step; for a closed gate, these slots must still be among what it misses |
  | `route` | the route `forward_request` picked last (`"billing"`), or `null` for none |
  | `sub_agents` | `{called?: [route], not_called?: [route]}`, by route name |
  | `bound` | `[{route, name, equals}]`: the route was dispatched, and its `bind` resolves `name` to `equals` |
  | `tools` | `{called?: [tool], not_called?: [tool]}`: allowed `tool_call` decisions of the run, `set_<slot>` and `forward_request` included |
  | `answer` | `{contains?: [text], not_contains?: [text]}`, case-insensitive, on the last answer as delivered (after the output filter) |
  | `filter` | `passed`, `withheld` or `redacted`: what the output filter did to the last answer |
  | `finished` | `true`: the last turn ended with an answer; `false`: it did not |

  `bound` reads the values from the final state with the same resolver
  dispatch uses, so a state change after the dispatch can differ from what was
  passed. `tools` and `sub_agents` read the audit rows of the whole
  conversation, nested sub-agents included.
- **Rubric.** Optional free text per case. After the deterministic checks, one
  non-streaming call on the agent's main model, as its principal, grades the
  visitor messages and the agent's answers (nothing else: no slot values, no
  tool results) as `{passed, reason}`. It is reported as `report.rubric`
  (`verdict`: `passed`, `failed`, `error`, `skipped`) and counted apart in the
  run's `rubric`. It never changes a case's `passed`, a run's `green` or the
  publish guard.
- **Goal-Plan-Action report.** Each case yields `{passed, error, goal, plan,
  action, rubric, turns, debug}`; each section is `{passed, checks: [{check,
  passed, expected, actual, message}]}`.
  - *Goal*: `finished`, `answer.*`, `filter`: did the conversation end as meant.
  - *Plan*: `gates.*`, `route`: did the gates hold and the router choose right.
  - *Action*: `sub_agents.*`, `tools.*`, `bound`: were the right things called.

  `passed` holds when the script ran through and every check in the three
  sections holds. `turns` carries each message with its status and answer;
  `debug` is the test chat's debug payload after the last step ([#90](#what-90-built)).
- **Running.** Each case is its own conversation through `run_draft_turn`, the
  test chat's door: version 0 (`DRAFT_VERSION`), the agent's grants, gates,
  binds and budgets, its tools really running. A version is tested by passing
  its stored spec as the draft, so no second execution path exists and nothing
  about "live" is overridden. A sub-agent a case dispatches to runs its live
  version, as in the test chat. Cases run one after the other in the order
  they were created.
- **Test data.** Version-0 conversations are what analytics leave out
  ([#100](#what-100-built)) and what retention sweeps like any conversation; a
  stored result keeps its report after the conversation is swept (`session_id`
  carries no foreign key). The `agent_tests` integration suite runs a suite and
  shows the analytics response unchanged and the conversation at version 0.
- **API** (`aiplane-api::pages::json_agent_tests`; the share rules of the other
  agent routes: `read` lists, `write` writes and runs, admins hold both):

  | Method | Path | Share | Purpose |
  |---|---|---|---|
  | GET | `/api/v0/agents/{id}/tests` | read | `{cases, latest_draft_run, latest_draft_run_current}` |
  | POST | `/api/v0/agents/{id}/tests` | write | create `{name, script, expect, rubric?}`; 201, 409 on a taken name |
  | PUT | `/api/v0/agents/{id}/tests/{case}` | write | replace a case |
  | DELETE | `/api/v0/agents/{id}/tests/{case}` | write | delete a case; 204 |
  | POST | `/api/v0/agents/{id}/tests/run` | write | `{source: "draft" \| "version:N"}`: run every case, synchronously; 201 with the stored run and its results. 400 without cases or with another `source`, 404 for an unknown version |
  | GET | `/api/v0/agents/{id}/test-runs` | read | the newest 50 runs, without results |
  | GET | `/api/v0/agents/{id}/test-runs/{run}` | read | one run with a result per case |

  A run is `{id, source, version, started_by, started_at, finished_at, passed,
  failed, green, rubric?, results?}`; `green` means no failed case and at least
  one passed. `passed` and `failed` count the deterministic result only.
- **Publish guard.** `publish.require_passing_tests: true` (a boolean in the
  spec, validated on save) makes `POST …/publish` answer `422
  agent_tests_failing` unless the newest draft run is green **for the draft and
  the suite as they are now**: each run stores a hash of the spec it ran and of
  the cases, so editing either makes the last run stale and the message says
  to run the suite again. With failing cases, `error.failing` lists `[{case_id,
  case_name, problems}]` with the failed checks in words; with no cases or no
  matching run it is empty. Rolling back (`/live`) is not guarded: it publishes
  nothing new.
- **Deviations.** A run is synchronous, like the test chat: the request returns
  when the suite is done. A case cannot write state before the first message.
  The judge's call writes no usage row.
- **Tests.** `crates/aiplane/tests/it/agent_evaluation.rs` runs a passing
  suite, a failing gate and route expectation, a trusted write with a bound
  value, a refused trusted write, the filter outcome, a version run, the
  publish guard through its whole cycle (no cases, no run, failing, edited
  suite, green, changed draft), the rubric reported apart, validation, the
  share rules, and analytics unchanged by a run. Parsing and judging are unit
  tests in `eval.rs`.

### What #102 built

A published agent served to other agent platforms over **A2A**, following
the Linux Foundation's *Agent2Agent (A2A) Protocol Specification v1.0.0*
(`a2a-protocol.org`, `specification/a2a.proto`, package `lf.a2a.v1`), JSON-RPC
binding (§9). Migration `0077_agent_builder.sql`. The handlers are
`aiplane-api::pages::a2a`; the spec section, the card and the state mapping
are `aiplane-runtime::agents::a2a` and `agents/spec/a2a.rs`.

- **Opt-in.** `publish.a2a: { enabled: true, skills?: [{id, name,
  description, tags?, examples?}] }`, validated on save: `enabled` is a
  required boolean, at most 20 skills, ids are slugs and unique. It is read
  from the agent's **live** version, like the limits: publishing a version
  without it stops serving at once. Without `skills` the card derives them:
  one for the agent as a whole (its principal's description, tagged with the
  route names) and one per route that has a `description`; a route without one
  is internal and not advertised.
- **Agent card** — `GET /a2a/agents/{id}/agent-card.json`, public, `404`
  unless the agent is enabled, published and opted in. `name` is
  `profile.display` (else the principal's display), `version` the live
  version, `supportedInterfaces` one `JSONRPC` interface at
  `{public_url}/a2a/agents/{id}` with `protocolVersion: "1.0"`,
  `capabilities: {streaming: true, pushNotifications: false,
  extendedAgentCard: false}`, `defaultInputModes`/`defaultOutputModes`
  `text/plain`, `securitySchemes: {aiplaneSystemToken:
  {httpAuthSecurityScheme: {scheme: "Bearer", bearerFormat: "gws_"}}}` with a
  matching `securityRequirements` entry. `Cache-Control: max-age=300` and an
  `ETag` of id and version.
  - *Discovery.* One gateway serves many agents, so there is no
    `/.well-known/agent-card.json`: the per-agent card URL is the one to
    configure in a client or a registry.
  - *Deferred: card signing* (`signatures`, JWS per §8.4). It needs a
    gateway signing key, its publication (a JWKS) and canonicalisation
    (RFC 8785); not trivial, so the card is unsigned and served over TLS.
- **Callers** (`docs/auth.md`). A caller is a system principal with a `gws_`
  token whose principal holds the new grant kind **`a2a_caller`** with the
  agent's id as `ref`. Default deny: a token without that grant, or with one
  for another agent, gets `403 PERMISSION_DENIED`; no token or a bad one
  `401` with `WWW-Authenticate: Bearer`; a person's `gwk_` token `403`.
  Granting `a2a_caller` follows #77's grant-time cap: the manager needs
  `write` on that agent by the one access rule (§2 "Shares": admins hold it,
  anyone else needs `can_manage_agents` and a `write` share), since letting
  another platform call the agent changes the agent. A token a manager
  minted loses the grant once the manager no longer holds that. The caller gets nothing of the
  agent's: the task runs as the agent's principal, with its grants.
- **Version.** Every request must carry `A2A-Version: 1.0` (header, or the
  `A2A-Version` query parameter). A missing header means 0.3 per §3.6.2, so it
  is refused like any other version with `-32009 VersionNotSupportedError`.
  The 0.3 wire (`message/send`, lowercase states, `kind` fields) is not
  spoken; `message/send` answers `-32601`.
- **Mapping.**

  | A2A | AIplane |
  |---|---|
  | context (`contextId`) | a root conversation owned by the agent's principal (`chat_sessions.id`), with an `a2a_contexts` row naming the caller, its token and client IP. Only that caller finds it; anyone else gets "not found". Pinned to the version live when it was opened, like a visitor's |
  | task (`id`) | one assistant turn of that conversation, and the user turn before it |
  | `SendMessage` without `taskId` | a new turn: in a new context, or in the caller's `contextId` |
  | `SendMessage` with `taskId` of an `INPUT_REQUIRED` task | the answer to a `secure_input` pause, through the same `agents::resume::claim` / runner `resume` as `POST /api/v0/embed/resume` (`ResumedBy::Participant`), so a verifier's code goes to the tool and nowhere else (not the transcript, the task, the model) |
  | `TASK_STATE_WORKING` | turn `in_progress`, or terminal while the runner still holds it (the output filter has not ruled). Per task: the claim names the turn holding the context (`SessionWorkers::holds`), so a finished task reads as finished — and `CancelTask` on it is `-32002` — while a later task of the same context runs |
  | `TASK_STATE_COMPLETED` | `completed`; the answer is the artifact `answer` and the last `history` message |
  | `TASK_STATE_FAILED` | `errored`; `status.message` is the generic `embed-error-generic` text, never the upstream's |
  | `TASK_STATE_CANCELED` | `cancelled` |
  | `TASK_STATE_INPUT_REQUIRED` | `suspended`, any kind. `metadata.aiplane` says `{kind, answeredBy: caller \| staff, requestId, expiresAt}`; `status.message` is the tool's message, or `agent-embed-decision-for-staff` for an approval or a handoff |
  | `CancelTask` | running: the conversation's stop flag (below), then the turn ends `cancelled`; paused: every level of the pause is cancelled (`cancel_suspended_turn`); terminal: `-32002` |
  | `GetTask` / `historyLength` | the task built from the turns; `0` omits `history`, `n` keeps the last `n` |

  Blocking is the default (§3.2.2): `SendMessage` returns once the task is
  terminal or `INPUT_REQUIRED`; `returnImmediately: true` returns the
  `WORKING` task at once. The run is spawned either way, so a caller that
  hangs up does not stop its task.
- **Streaming maps cleanly onto buffered answers.** `SendStreamingMessage`
  and `SubscribeToTask` answer `text/event-stream`, one JSON-RPC response per
  `data:` frame: the `task` first, then — once the task is no longer working —
  its whole answer as one `artifactUpdate` (`lastChunk: true`) and a
  `statusUpdate` with the final state; a paused, failed or cancelled task gets
  the `statusUpdate` alone. `: working` comment lines every 15 s; the stream
  closes after 10 minutes, and the caller re-subscribes or polls.
- **Exactly like an embed visitor** — one path, not a fork:
  - the turn is opened like `/api/v0/embed/messages` and run by the installed
    `AgentTurnRunner` (`LiveAgentRunner`), so grants, gates, binds, budgets,
    suspend/resume, the output filter (#89) and the inbox (#96) apply as they
    do there;
  - **admission** is `agents::embed::admit` with `Admission { a2a_context, ip
    }`: the live spec's `publish.rate_limits.visitor` counts the admitted
    messages of one context — the one that opened it too, counted once the
    context has an id (`embed::Admitted::opened`) — and `…ip` counts every
    admission from a client IP, embed ones included, and the owner budget
    and operator `system` limits apply. A refusal is a
    JSON-RPC error `-32000` with `RATE_LIMITED` or `AGENT_UNAVAILABLE`, a
    `Retry-After` header and the Fluent message, audited as `limit_refused`;
    nothing is stored and nothing runs;
  - **retention** sweeps the conversation, and `a2a_contexts` goes with it.
- **The caller in the call chain.** `RunChain` gained `caller:
  Option<RemoteCaller {protocol: "a2a", principal_id, name, token_id}>`
  (serialized only when set, so a visitor's chain reads as before), carried
  into sub-agent chains. `OpenedTurn.caller` sets it; `agents::resume`
  rebuilds it from `a2a_contexts`, so a resumed task names its caller too.
  Every `tool_call`, `run_suspended`, `run_resumed`, `output_blocked`, usage
  and `mcp_tool_audit` row of the task therefore names the caller. Each
  started, answered or cancelled task is also an `agent_audit` row
  `a2a_task` on the agent: `{action: message | input | cancel, context_id,
  task_id, caller_id, caller_name, token_id}`, never the text.
- **Stopping a running agent turn.** An agent turn runs on a worker of the
  session worker registry, like a person's chat turn: `agents::embed::claim(
  workers, principal, session, turn)` registers it, keyed by the principal
  that owns the conversation, and `SessionWorkers::cancel_turn` stops only
  the turn holding it. A message queued behind a decision runs under the
  resumed turn's claim, which `SessionWorkers::hand_over` passes on to the new
  turn. `headless::drive` runs a root turn on the worker that claimed it (its
  cancel flag and channel); any other agent turn — a sub-agent's child run,
  or a root turn nobody claimed (the test chat, an evaluation) — registers a
  worker of its own while it runs, and still stops by its root
  conversation's flag, so a cancel reaches sub-agent runs too. `CancelTask`
  and shutdown (`cancel_all`) set it.
- **Errors.** JSON-RPC 2.0 envelopes (§9.5): `error.data` is one
  `google.rpc.ErrorInfo` whose `reason` names the error. A2A's codes where
  they apply (`-32001` task not found, `-32002` not cancelable, `-32003` push
  notifications, `-32004` unsupported — `ListTasks`, `GetExtendedAgentCard`,
  a message to a terminal task —, `-32005` a non-text part or output mode,
  `-32009` version); the standard ones for parse, envelope, method and params;
  and `-32000` (implementation-defined, unassigned by A2A) for AIplane's own:
  `UNAUTHENTICATED`, `PERMISSION_DENIED`, `AGENT_NOT_SERVED`,
  `RATE_LIMITED`, `AGENT_UNAVAILABLE`, `TASK_IN_PROGRESS`, `CONTEXT_WAITING`
  (a new message while the context waits on a task), `DECISION_FOR_STAFF`
  (the caller tried to answer an approval or handoff), `NOT_WAITING`.
  Authentication errors are HTTP 401/403, an unserved agent 404; every other
  error is HTTP 200, as JSON-RPC over HTTP expects.
- **Deviations and not built.**
  - Only text parts in and out (`ContentTypeNotSupportedError` otherwise); no
    files or structured data.
  - No push notifications, no `ListTasks`, no extended card; the card says
    so in `capabilities`.
  - A message into a context that waits on a task is refused
    (`CONTEXT_WAITING`) rather than queued as the widget's is: an A2A message
    without a `taskId` is a new task, and that task would have no id until the
    pause is settled.
  - The client's `messageId` is not stored; `history` messages carry the turn
    ids. `referenceTaskIds`, `extensions` and `metadata` are accepted and
    ignored.
  - A `CancelTask` of a running task waits up to 15 s for the run to notice
    the flag (between rounds or upstream chunks) and returns the task as it is
    then.
- **Tests.** `crates/aiplane/tests/it/a2a.rs` on wiremock upstreams: the card
  only for an opted-in, published agent (and not for an opted-in draft);
  missing, bogus, unscoped and other-agent tokens refused with nothing run;
  the `a2a_caller` grant cap; version and envelope errors; a completed task
  with the agent's answer, its context a principal-owned conversation and the
  `a2a_task` audit row; a second task in the same context that replays the
  first; context/task mismatch and a message to a finished task; `GetTask`
  and `historyLength`, refused to another caller; cancelling a running and a
  paused task, and refusing a finished one; the output filter withholding an
  identifier, with the caller in the row's chain; a rate and a budget refusal
  as JSON-RPC errors; a secure input answered on the task (the code nowhere in
  the task or the model's input; the caller in `run_resumed`'s chain); an
  approval the caller cannot give; the streamed task, answer and status. Unit
  tests: `agents/a2a.rs` (opt-in, card, derived skills, state mapping),
  `agents/spec/a2a.rs` (validation), `pages/a2a/` (parts, configuration,
  versions, error shape), `aiplane-agents`' `db/a2a_contexts.rs`, `run_chain.rs`,
  `agents/embed.rs` (stop flags) and `aiplane-core/tests/migration_0077.rs`
  (the `principal_grants` kinds).

### What #101 built

An external agent as a route target: a route may hand the visitor's request
to another platform's agent that speaks A2A v1.0 (the JSON-RPC binding the
gateway's own server speaks, [#102](#what-102-built)). Code in
`aiplane-runtime::agents::a2a_client` (`guard`, `card`, the exchange) and
`agents/spec/route_kinds.rs` (validation); the waiting task in
`aiplane-agents::db::agent_a2a_tasks` (migration `0077_agent_builder.sql`).

```yaml
routes:
  partner:
    when: { all: [ { slot: issue, set: true }, { slot: verified, provenance: host } ] }
    task: "Warranty question: {issue}"
    bind: { customer: state.verified.customer_id }   # sent as one `data` part
    a2a:
      card_url: https://partner.example.com/.well-known/agent-card.json
      auth: { kind: bearer, token: "…" }              # sealed on save as token_sealed
      # or { kind: api_key, scheme?: <card scheme>, token }
      # or { kind: oauth_client_credentials, client_id, client_secret, scopes? }
      finish: { schema: { type: object, required: [answer], properties: { answer: { type: string } } } }
      budget: { seconds: 120 }                        # default 120, at most 900
```

- **Target kinds are keys.** A route's target is its one key besides `when`,
  `description`, `task` and `bind`: `agent`, `human`, `a2a` and `loop` ([#103](#what-103-built)). Everything a kind needs lives under that key, its
  own `finish` and `budget` included, so a builder that does not know a kind
  (#104's canvas) still finds a route's shared keys where they always are. A
  route naming two targets is one issue at `routes.<r>`.
- **What leaves the gateway.** `SendMessage` with a text part (the rendered
  `task`) and, when the route binds anything, one `data` part with the bound
  values (`{"customer": "K-12345"}`). Never the transcript, the slots or a
  word the model wrote. Headers: `A2A-Version: 1.0`, the credential, nothing
  else. `configuration.returnImmediately` is `false`; a task the peer still
  reports as submitted or working is polled with `GetTask` every 500 ms.
- **The card** (`card.rs`) is fetched from `card_url` and cached for five
  minutes per URL. It must have `name`, `capabilities` and a
  `supportedInterfaces` entry with `protocolBinding: JSONRPC` and a `1.x`
  `protocolVersion`; the first such entry is the endpoint, and its `tenant`
  is sent along. `securitySchemes` and `securityRequirements` decide the
  auth. Skills, modes and signatures are not checked; a signed card is not
  verified. The 0.3 card shape (`url`, `preferredTransport`) is not read.
- **Auth, limited to what a principal can hold.** `auth.kind` picks one of
  the card's schemes (by `scheme` name, else the first of its type):
  `bearer` → `httpAuthSecurityScheme` with `scheme: Bearer`; `api_key` →
  `apiKeySecurityScheme` in a header (a query or cookie key is refused, so a
  secret never sits in a URL); `oauth_client_credentials` →
  `oauth2SecurityScheme.flows.clientCredentials.tokenUrl`, a
  `client_credentials` form post, the token cached until 30 s before it
  expires. The cache key is the token URL, the client id, the SHA-256 of the
  secret, the sorted scopes and the agent's principal id, so a route with
  another secret (a wrong one included), other scopes or of another agent
  signs in itself and never rides on a token it did not earn. `token` and `client_secret` are sealed on
  every save (`a2a_client::seal_secrets`, next to the host-JWT secret) and
  stored as `token_sealed` / `client_secret_sealed`; a GET → PUT round trip
  keeps them. A card that requires auth when the route brings none, or offers
  no scheme of the route's kind, ends the route `incomplete` saying which.
  Not built: OpenID Connect, mTLS, authorization-code flows — each needs a
  person or a client certificate the principal does not have.
- **The grant.** *Chosen:* a new grant kind, `a2a_agent`, whose `ref` is the
  exact card URL (the `principal_grants` CHECK in migration `0077_agent_builder.sql`
  admits `… 'a2a_caller', 'a2a_agent'`). Reusing connector grants
  would mean an `mcp_catalog` row the MCP manager would try to connect to.
  The validator requires the grant (`routes.<r>.a2a.card_url`, with the
  `POST …/grants {"kind": "a2a_agent", "ref": …}` to make), and the dispatch
  checks it again, so a revoked grant sends nothing (`forwarded: false,
  reason: not_granted`). **Grant-time cap:** an external agent is nothing a
  manager holds, and the grant lets visitor-derived data leave the gateway, so
  only an admin may make it (`403 grant_exceeds_manager` otherwise); the ref
  must pass `check_card_url` (https, or http to a loopback host; no
  credentials or fragment in it).
- **SSRF** (`aiplane_core::server::outbound_guard`, `Policy::agent`; the
  same guard `fetch_url` and `load_image_url` use). The card URL, the
  endpoint the card names and the OAuth token URL are each resolved before
  every connection; every address
  must pass, and the request goes out on a client pinned to exactly those
  addresses (`resolve_to_addrs`), with redirects off, so a second DNS answer
  cannot swap in a private one. Always refused: unspecified, link-local
  (169.254.0.0/16 with the metadata endpoint, fe80::/10), broadcast,
  multicast, and their IPv4-mapped forms. Refused unless
  `$AIPLANE_ALLOW_PRIVATE_NETWORKS=true` (`Config.network`, environment
  only like `$AIPLANE_TRUSTED_PROXIES`): loopback, RFC 1918, 100.64.0.0/10,
  fc00::/7 and plain `http`. The MCP OAuth flow goes through the same
  guard under a policy of its own (`Policy::mcp_oauth`), which allows
  private ranges on purpose: an admin curates the MCP catalog.
- **One origin.** The endpoint and the OAuth token URL the card names must
  share the granted card URL's origin (scheme, host, port), or the route ends
  `incomplete` before anything is sent. The grant names the card URL, so a
  card that points elsewhere — a compromised CDN, a stale host taken over —
  must not collect the route's credential or the task's bound values.
- **Size caps** (`capped_read::read_capped_for`). Every body read from outside — the
  card (256 KiB), the JSON-RPC answer (1 MiB), the OAuth token answer
  (64 KiB) — is refused when its `Content-Length` is over the cap, before a
  byte is read, and otherwise read chunk by chunk and dropped the moment the
  running total passes it. A chunked body without a length therefore cannot
  make the gateway buffer more than the cap.
- **The result** is the first `data` object among the completed task's
  artifact parts (then its status message), else the first text part that
  parses as a JSON object (a fenced block too); a direct `message` answer is
  read the same way. It must pass the route's `a2a.finish.schema` (the #78
  subset validator), else the outcome is `incomplete` with reason `failed`
  naming the violations. So is no structured result, a task that ends
  `FAILED`/`REJECTED`/`CANCELED` (with the peer's status text, cut to 300
  characters), `AUTH_REQUIRED`, a JSON-RPC error, a non-2xx answer, an answer
  over 1 MiB, or a guard refusal. Running past `budget.seconds` is
  `seconds_exhausted`. The tool result is `{forwarded: true, route,
  remote_agent, outcome, note}`; the main agent's `Flag` policy screens it
  like a sub-agent's (an injection is flagged and audited).
- **`input-required`.** *Chosen:* the run pauses only when the peer's status
  message carries a `data` part — a structured request for input — and the
  run can pause. The call then suspends as `secure_input` with the catalog
  text `agent-a2a-input-required` (never the peer's own words, which a
  visitor would otherwise read as ours) and a 10-minute deadline, and records
  the remote task and context ids in `agent_a2a_tasks` keyed by the waiting
  turn and call. The visitor's value goes back as `{"data": {"value": …}}`
  on that `taskId`/`contextId` when `forward_request` runs again with
  `Decided(Value)` (it takes the row, so a task is continued once; without a
  row the value is a staff answer to a handoff, as before), and the driver's
  secure-input scrub keeps the value out of everything stored. Free-text
  `input-required` is `incomplete`: a route cannot hold a conversation with
  the peer, and the model must not answer it on the visitor's behalf.
- **Audit and debug.** `sub_agent_dispatched` and `sub_agent_finished` with
  `{route, target: "a2a", card_url, dispatch_id, resumed, remote_agent?,
  outcome?}` on the calling principal with its chain; the test chat's
  `debug.sub_agents` pairs them by `dispatch_id` (a sub-agent's by its child
  `turn_id`). #100's analytics count them as sub-agent dispatches.
- **Validation** (`spec/route_kinds.rs`, on save and publish): `task`
  required, `bind` as for a sub-agent route (and the same trusted-gate rule
  for state binds), `a2a` keys `card_url`/`auth`/`finish`/`budget` only,
  the card URL's shape and grant, `auth.kind` and its keys (exactly one of
  the plain or sealed secret, `client_id` for client credentials),
  `finish.schema` through `FinishContract::new` (required on publish), and
  `budget.seconds` in 1–900.
- **Tests.** `agents/run/tests/a2a.rs` runs a main agent against a wiremock
  A2A peer (card, JSON-RPC endpoint and token endpoint): the task and bound
  values sent and nothing of the transcript, the bearer header, the checked
  result returned and audited, the credential stored sealed only; a
  violating, a prose, a failed and a free-text `input-required` answer each
  `incomplete`; polling a working task; structured `input-required` paused
  and answered by the visitor, the value at the peer and nowhere else; a
  loopback peer refused without the switch, with nothing fetched; a revoked
  grant; a slow peer past `budget.seconds`; one OAuth token and one card
  fetch for two dispatches; an injection flagged. Pure halves in
  `a2a_client/tests.rs`, `guard.rs`, `card.rs`; the grant cap and the sealed
  save over HTTP in `tests/it/system_principals.rs` and `tests/it/agents.rs`.
- **Not built.** Streaming (`SendStreamingMessage`), push notifications,
  file parts, the 0.3 protocol, card signature checks, and answering a
  free-text `input-required`.

### What #103 built

A route kind that drafts, critiques and revises (Google ADK's LoopAgent
pattern): a worker sub-agent and a critic sub-agent take turns until the
critic accepts. Code in `aiplane-runtime::agents::router::loop_route` (the
run) and `agents/spec/route_kinds.rs` (validation). No migration.

```yaml
routes:
  offer:
    when: { slot: issue, set: true }
    task: "Write an offer for: {issue}"
    bind: { customer: state.verified.customer_id }   # passed to both, as route.<name>
    loop:
      worker: <agent id>                  # finishes with the result
      critic: <agent id>                  # finishes with { accepted: boolean, feedback?: string }
      max_iterations: 3                   # default 3, at most 10
      budget: { seconds: 300, tokens: 60000 }   # caps the sum of every child run; optional
```

- **One iteration** is a worker child run, then a critic child run, each a
  sub-agent run exactly as a sub-agent route starts one (`run_child`, shared
  with it): its own principal, live version, grants, finish contract and
  `main.budget`, a child session under the main turn, the call chain
  extended. Neither sees the transcript:
  - the worker's first task is the route's rendered `task`; from the second
    iteration on, that task plus its previous result and the critic's
    `feedback`, both marked as data;
  - the critic's task is the original task and the worker's result, with
    the instruction to set `accepted` and say in `feedback` what to change.
- **Stopping.** The loop ends when the critic's result has `accepted: true`
  (`stopped: accepted`), after `max_iterations` (`max_iterations`), when the
  route's budget is spent before the next child run (`budget`), or when a
  child run ends `incomplete` (`worker_incomplete`, `critic_incomplete`).
- **The route budget caps the sum.** `seconds` counts from the start of the
  loop; `tokens` counts every round of every child run, and of anything they
  dispatch: `RunOptions.spend` carries a `SpendMeter` into the children's
  runs, and the driver adds each round's tokens to the meter of the agent
  run it drives. Each child's own budget is tightened to what is left
  (`Budget::capped`), so a child ends on its own `seconds`/`tokens` limit
  rather than being cut off; the loop checks the remainder before every
  child run. Without `budget` only the children's own budgets and
  `forward_request`'s 15-minute ceiling apply.
- **What returns to the main agent** is the worker's last result:
  `{forwarded: true, route, loop: {worker, critic, iterations, accepted,
  stopped}, outcome, note}`. `outcome` is the worker's last `finished`
  result even when the critic never accepted it (`accepted: false` says so),
  the worker's own `incomplete` when it did not finish, or `incomplete` with
  `tokens_exhausted`/`seconds_exhausted` when the budget ran out before any
  result. The main agent's `Flag` policy screens it like any tool result.
- **A child that pauses** (an approval, a secure input) is withdrawn
  (`cancel_suspended_turn`) and counts as `incomplete`. *Chosen:* the next
  step of the loop needs its result now, and resuming a loop mid-iteration
  would need the loop's own state stored with the pause. Agents with
  `always_ask` tools make poor workers and critics.
- **Audit.** Every child run writes `sub_agent_dispatched` and
  `sub_agent_finished` with `loop: {route, iteration, role: worker|critic}`;
  each critic verdict writes `loop_iteration` (`{route, iteration,
  accepted, feedback}`); the end writes `loop_finished` (`{route, worker,
  critic, iterations, accepted, stopped, tokens}`). All on the calling
  principal with its chain.
- **Test-chat debug view.** `debug.sub_agents` lists every worker and
  critic run with its outcome, and the SPA's debug panel badges each with
  its role and iteration (`agents-debug-loop-worker`/`-critic`);
  `debug.loops` lists the `loop_iteration` and `loop_finished` rows in order,
  each with its `event`.
- **Validation** (save and publish): `task` required, `bind` as for a
  sub-agent route; `loop` keys `worker`, `critic`, `max_iterations`,
  `budget` only. `worker` and `critic` go through the sub-agent check
  (existing agent, not the agent itself, live to publish) and the graph
  check: `routed_agents` now yields a loop's worker and critic, so a loop
  that reaches the main agent again is a cycle at `routes.<r>.loop.<role>`
  and counts toward the depth of 3, and a worker or critic that binds a
  tool demands a trusted gate. The worker and the critic must differ.
  Against the critic's live version, its `finish.schema` must list
  `accepted` as a required `boolean` (`feedback`, if declared, a
  `string`); against both, the route passes exactly the `route.<name>`
  values either binds; on publish the worker needs a `finish.schema`.
  `max_iterations` is 1–10, `budget.seconds` 1–900, `budget.tokens` ≥ 1.
- **Tests.** `agents/run/tests/loop_route.rs` on wiremock models: the critic
  rejects once with feedback and accepts the revision (tasks and feedback as
  each child saw them, no transcript, audit rows); `max_iterations` reached
  with the last draft unaccepted; the route's token budget spent by the
  worker before the critic runs; a worker that never finishes; the test
  chat's `debug` showing worker 1, critic 1, worker 2, critic 2 and the loop
  rows; the validator refusing a critic without a boolean `accepted`, a
  worker that is also the critic, the agent itself, a critic that routes
  back (cycle) and out-of-range settings. `tests/it/agents.rs` saves a loop
  route through the JSON tab's `PUT …/draft`.
- **Not built.** A configurable acceptance field name, a separate critic
  rubric outside its spec, parallel workers, and resuming a paused child.

### What #111 built

A complete, tamper-evident activity log: everything an agent does, enough to
reconstruct any conversation, decision and model exchange afterwards, with
no secret in it. Migration `0077_agent_builder.sql`; the log is
`aiplane-agents::db::agent_audit`, the runtime's door to it
`aiplane-runtime::agents::audit`.

**Shared mechanisms.**
- *Used:* the typed `AgentSpec` (`publish.audit_retention_days`), the
  session worker registry and `RunChain` for correlation, `withhold_secret`
  and `sensitive_args` for secrets, the retention sweeper of #92.
- *Introduced:* the activity log itself — one event writer
  (`agent_audit::append`), one runtime door (`agents::audit::record_event`
  and the wrappers `record` and `ToolContext::audit`), one hash-chain
  verifier. `activity_events_are_written_only_through_the_activity_log`
  (`docs/testing.md` → "Architecture tests") fails on any other `INSERT`,
  `UPDATE` or `DELETE` of `agent_audit` and on any other call of its
  writers.

**One log, not two.** `agent_audit` (#77) became the log rather than gaining
a sibling: the decision events it already held (`tool_call`,
`route_decision`, `run_suspended`, …) and the management events (grants,
shares, publishes) are events of the same log, with the same columns and
the same chains. The table keeps its name because #100's analytics, the
test-chat debug view and the evaluation read it.

**Columns.** Every event carries its correlation ids as columns, not only
in `detail`: `agent_id` (the main agent at the root of the run — a
sub-agent's events belong to the agent whose conversation they happened
in), `principal_id` (who acted: the running frame), `version`,
`conversation_id` (the root conversation), `session_id` (the run's own
session: a sub-agent's child session), `turn_id`, `round`, `call_id`,
`visitor_id`, `caller_id` (the A2A caller), `actor_id` (the person behind a
management change or a staff decision), `duration_ms`, `chain` (the
serialized `RunChain`), `created_at`, `kind` and `detail`.

**Hash chains.** *Chosen:* one chain per conversation, sub-agent runs
included (`chain_key = conversation:<root session>`), and one per agent for
everything outside a conversation (`agent:<principal id>`: management
changes, refused visitors, sweeps). **No writer picks the chain:**
`agent_audit::append` resolves it from the event's own `session_id`,
following a child session's `parent_turn_id` up to the root conversation
(whose owner is also the event's `agent_id` when no run chain says so), so
a state write, a host identity or an A2A task names only the session it
happened in and lands where the rest of the conversation is; an event with
no session belongs to its run chain's root, or else to the agent's own
chain. Per conversation rather than per agent
because retention removes conversations, and a chain must go whole or not
at all; and because parallel conversations then never contend for one
chain head. Within a chain events count up from 1 (`seq`); each stores the
hash of the one before (`prev_hash`) and its own `hash` = HMAC-SHA256 over
its canonical JSON — every column except `hash` and the rowid, keys sorted,
no whitespace, `chain` and `detail` as the exact stored text — under the
log key its `key_id` names. A unique index on `(chain_key, seq)` refuses a
fork. Every event has a chain, so `verify` reports an event outside every chain as inserted outside the
gateway.

**The log key.** No new secret: the key is derived from the gateway's
at-rest key (`Crypto`, `$AIPLANE_ENCRYPTION_KEY` or the session secret) as
HMAC-SHA256(at-rest key, `croit-aiplane/activity-log-chain/v1`), and
`Crypto::activity_keys` returns the ring — the current key first, then one
per retired at-rest label. Each event stores the `key_id` (the first 16 hex
digits of the key's SHA-256: a fingerprint, not the key) it was signed
with, and `verify` picks that key from the ring, so a label rotation (the
supported way to rotate the at-rest key, `crypto.rs` → `RETIRED_LABELS`)
leaves every older event verifiable. Replacing `$AIPLANE_ENCRYPTION_KEY`
outright keeps no old key — sealed secrets stop opening too — and `verify`
then reports the older events as signed with a key the gateway does not
hold. The ring is installed process-wide when the gateway's state is built
(`AppState::new`, `with_crypto`), because every writer, the management
changes in `aiplane-agents` included, extends the same chains — and those
record on a caller's transaction with a connection, not the gateway's state,
so passing the ring explicitly would thread it through every management
accessor and its handler. The ring is shared (`Arc<[ActivityKey]>`) and each
key holds its keyed HMAC state, so a signature costs no key schedule. An event
written by a process without a ring (a unit test, a CLI) carries `key_id =
unkeyed` and a plain SHA-256; where a ring is installed, `verify` refuses
it, so rewriting a chain "unkeyed" does not pass either.

**Anchors.** When a turn of a conversation ends — paused or answered, at
the end of `drive_opened_from`, after the output filter — the conversation
chain's head (`seq`, `hash`) is written into the agent's own chain as a
`chain_anchored` event, read and written in one write transaction
(`agent_audit::anchor_conversation`, through `agents::audit::anchor`,
bounded like any event; a failure is logged and the conversation's newest
events stay unanchored until the next turn). The retention sweep deletes a
chain and writes `activity_swept {chain_key, events, before}` into the
agent chain in the same transaction, so the removal is recorded with it.

`agent_audit::verify(agent)` walks the agent's own chain first, collecting
the latest anchor per conversation and the sweep markers, then every
conversation chain, in batches of 256, and reports the first thing that
does not hold: a `seq` gap (an event removed from the middle), a
`prev_hash` that is not the previous hash (an event inserted or relinked),
a `hash` that does not match the content under its key (an event changed,
or re-hashed without the key), an unkeyed or unknown-key event, a
conversation chain that ends before its anchored `seq` or whose event at
that `seq` is not the anchored one (its tail was cut or rewritten), and an
anchored chain that is gone without a sweep marker (a conversation's log
deleted inside its retention), and an event outside every chain. It also
reports `unanchored` (conversation events newer than their chain's latest
anchor) and the agent chain's `head` (`seq`, `hash`). The anchored event is
read by its `(chain_key, seq)` index, not found by walking.

**Verification watermarks** (`agent_audit::verification`, migration
`0077_agent_builder.sql`). *Chosen over re-walking every time:* a
chain that checked out is remembered in `activity_verified` — its `seq`
and `hash` and, for the agent's own chain, the anchors and sweep markers
read so far — signed under the log key like an event (`key_id`, `mac`).
`verify` resumes every chain from its watermark, so a check hashes what
was written since the last one (`checked`), while `events` still counts
everything the chains hold; `verify_full` (`?full=true`) walks every chain
from its start. A watermark that is unsigned, signed with a key the ring
lacks or not matching its signature — one the database alone moved — is
ignored, and so is one whose event is gone or no longer has the stored
hash (a chain cut or rewritten at it); either way the chain is walked
whole. The cost: a change *below* a watermark (an event rewritten with its
hash column untouched, one deleted from the middle) shows only on a full
walk — run `?full=true` periodically, as with the pinned `head`. The
sweep deletes a conversation chain's watermark with the chain.

*Residual limits.* Someone holding the at-rest key (or the session secret
it is derived from) can forge anything — the key protects against a
database-only attacker, not against the gateway's operator. A database-only
attacker can still cut the agent chain's own tail together with the
conversation events those last anchors covered, and delete events written
after the last anchor (`unanchored`); both show only against a copy of the
`head` kept outside the gateway, which is why `verify` returns it — an
operator who exports the log or pins the head regularly (cron, SIEM)
bounds what can disappear unnoticed to what was written since. Deleting a
whole agent's rows, chain and anchors alike, leaves nothing to compare
against except that pinned head.

**Writing: synchronous, bounded, fail closed.** *Chosen over a bounded
queue:* every event is written before the run moves on, in a write
transaction of its own (`BEGIN IMMEDIATE`, so concurrent writers to one
chain queue on SQLite's lock instead of racing for its head) or — for a
management change and a state write — on the change's own transaction, so
the change and its event commit together or not at all. Either way the
transaction is a `db::WriteTx`, which only `WriteTx::begin` (`BEGIN
IMMEDIATE`) makes and which `agent_audit::append` requires: a
chain head, a slot's old value or a rate window is read under the write
lock it is then written under, never in a deferred transaction that takes
the lock only at its first write. WAL with
`synchronous = NORMAL` makes a commit a page write, not an fsync, so this
costs a turn about a millisecond per event; nothing waits in memory, so
nothing is lost on a crash or a shutdown and there is no queue to flush.
**Bound:** a run event may take at most `agents::audit::WRITE_BOUND` = 5 s
(lock wait included). **Fail closed:** an event of an agent run that could
not be written marks the run (`AgentRun::mark_log_failed`); the driver
stops it before the next model round and before a batch of tool calls
runs, errors its turn with `agents::audit::LOG_UNAVAILABLE`, and a turn
that had already answered when its `turn_finished` could not be written is
errored after the fact, so the visitor never gets an answer the log does
not show. The failure is logged at `error` with the event's correlation
ids. Outside a run (a refused visitor, a sweep marker) the failure is
logged and the request goes on as it would have.

**Hook points.** As low as the code allows, so nothing reaches around them:

| Event | Written by | Detail |
|---|---|---|
| `llm_exchange` | the driver's round loop (`openai_driver/exchange.rs`), around the upstream call, whichever way the round ends | `purpose: round`, `round`, `model`, `real_model`, `backend`, `request` or, after a turn's first round, `request_delta` (see "Storage") (the body exactly as sent: system message, messages, tool offer, parameters — but for what the log never keeps, below), `response` (`status`, `content`, `reasoning`, `tool_calls`, `finish_reason`, `usage`), `latency_ms`, `error` (no backend, transport, non-2xx with the full body, stall, loop), `cancelled` |
| `llm_exchange` | a constrained choice on a model (`agents::model_call`): the route classifier, the topic guard (#115) | `purpose: route_classifier` or `scope_guard`, `model`, `backend`, `request`, `response`, `answer`, `error` |
| `scope_decision` | the topic guard (`agents::topic_guard`, #115) | `verdict` (`in_scope`, `out_of_scope`, `failed`), `topics`, `model`, `error` |
| `tool_call` | the call policy (#83, unchanged) | `decision`, `policy` |
| `tool_result` | the tool runner (`execute_tool_call`), for every call including an unregistered tool and a refused repeat; the resume path for a denied call and a sub-agent's result | `tool`, `arguments` (as the model wrote them; `{redacted: true}` for a tool that declares `sensitive_args`), `status` (`completed`, `failed`, `invalid_args`, `timed_out`, `unregistered`, `refused_repeated`, `denied`, `answered_by_sub_agent`), `result` (the tool's whole answer before injection screening and before the prompt's byte budget trims it), `injection` (`policy`, `signals`); `duration_ms` |
| `state_written` | `agent_state::put`, on the write's transaction, in the chain of the written session's root conversation (a sub-agent's slot in its child session included, whichever door wrote it) | `slot`, `old` (`value`, `provenance`, `set_at`, or `null`), `new`, `provenance` (who wrote it: `llm`, `verifier:<id>` or `host`), `set_at` |
| `turn_started` / `turn_finished` | `headless::drive`, for every agent turn (main and sub-agent, resumed too) | the message the turn answers (a visitor's, or a sub-agent's task), or `resumed: true`; `status`, `answer`, `error`, `outcome` (a contracted run's `RunOutcome`: budget, rounds, repeated call …) |
| `route_decision`, `sub_agent_dispatched`/`_finished`, `loop_iteration`/`_finished` | the router (#87/#88/#103) | as before (every route's gate, the route picked), plus the `method` that picked it (`rules`, `only_open`, `classifier`); an A2A dispatch also records the `message` it sent (`{secure_input_sent: true}` for an answer to the peer's `input-required`) |
| `run_suspended`, `run_resumed`, `human_handoff` | the pause and resume paths (#82, #96) | `run_resumed` also carries a staff `answer` to a handoff, and only `secure_input_received: true` for a secure input |
| `verifier_outcome`, `host_identity`, `output_blocked`, `limit_refused`, `a2a_task`, `injection_detected` | as before (#95, #89, #92, #102, #93) | `output_blocked` now also keeps the withheld `original` and what was `delivered` |
| management kinds | the agent DB modules, on the change's transaction | as before |
| `assist_suggested` | the prompt assistant (`agents::assist`, #117), in the agent's own chain | `action`, `scenario`/`template` or `field`/`text`, `model`, `usage`, `offered`, `dropped`, `error` ([#117](#what-117-built)) |
| `activity_swept` | the retention sweep, on the deletion's transaction | `chain_key`, `events`, `before` |
| `chain_checkpoint` | the retention sweep, when it cuts the agent's own chain | `base_seq`, `base_hash`, `removed`, `seqs`, `from`, `to`, `before`, `anchors` |
| `chain_anchored` | the end of every turn (`drive_opened_from`) | `chain_key`, `seq`, `hash` of the conversation chain's head |
| `llm_exchange` | compaction, the rubric judge, the vision fallback | see below |

`for_principal` — the decision trail the agent's GET, the test-chat debug
view and the evaluation read — leaves out the content kinds
(`AuditKind::is_content`: `llm_exchange`, `tool_result`, `turn_started`,
`turn_finished`); they are read through the activity API.

**Secrets never enter it.** The log redacts at its one choke point:
`agent_audit::append` applies `agent_audit::Redaction` to every event's
detail before it is hashed, by event kind — the arguments of a call to a
tool that declares `sensitive_args` become `{redacted: true}` in a
`tool_result` or `tool_call`, and in every tool call an `llm_exchange`
carries (the answer's `tool_calls`, and the assistant `tool_calls` of
every later request or request delta); a turn's secure input becomes
`[secure input withheld]` in an event of any kind. No writer redacts for
itself, so none can forget to: each event of a run carries the run's
`Redaction` (`ToolContext::redaction`, attached by `ToolContext::audit` /
`audit_event` and `RunLog`), which the run collects as it goes — the
driver adds the secure input a turn resumes with, and the runner, the
exchange log and the resume path note every tool they meet that declares
its arguments sensitive (`AgentRun::note_sensitive`). A one-time code
reaches the verifier through `Suspend::Decided(SecureInput, Value)` only —
the decision carries the kind of pause it settles, and one rule,
`Redaction::decided`, says what is withheld: a secure input's value, never
an approval or a human's answer (which `run_resumed` records). The same
rule withholds it from what goes elsewhere than the log
(`Redaction::withhold`: the resumed call's result the model reads, the
verifier's connector answer). The run's resume records
`secure_input_received` instead of it, the verifier's MCP check is
redacted as before (#95), and the model never saw it. A stored request
therefore differs from the one sent exactly there. The A2A credential is sealed in the spec and
sent only as a header, which no event records. Tokens, embed keys and
client secrets are hashed or sealed where they are stored and never part
of an event. `activity::a_whole_run_is_one_hash_chain_that_reconstructs_it_and_holds_no_secret`
(`agents/run/tests/activity.rs`) runs a conversation through a closed
gate, an OTP verifier fed a known code, a sub-agent, an external A2A agent
with a sealed bearer token and a handoff to a person, then greps every text
column of every table for the code and the token.

**Model calls outside the round loop** go through the same door as an
`llm_exchange` with their own `purpose` (`agents::audit::SideExchange`,
recorded for an agent's run only — `RunLog::of` is `None` for a person's
turn, so a person's chat records nothing here and its logging is
unchanged):
- `compaction_summary` — the conversation's compaction summary, when the
  compacted conversation is an agent's (`maybe_autocompact` gets the run's
  `RunLog`), in that conversation's chain;
- `rubric_judge` — the evaluation's rubric judge (#99), in the case's test
  conversation (version 0);
- `vision_fallback` — each call to the fallback vision model that
  describes an image a tool returned for a model that cannot see
  (`capabilities::maybe_replace_image_content` now returns its calls), with
  the tool call's `call_id`.

Each records `model`, `backend`, the `request` exactly as sent (the image
included), `response` (`status`, `body`), `latency_ms` and `error`.

**Retention.** `publish.audit_retention_days` (typed, default **365**,
read from the live version like `retention_days`): the hourly sweep deletes
the *whole* chain of a conversation that no longer exists and whose newest
event is older than that, never part of a chain, and appends an
`activity_swept` marker per chain to the agent's own chain, in the same
transaction. The validator refuses a
value below `publish.retention_days` (default 30), so a conversation's log
always outlives the conversation.

The agent's own chain honours the same retention, so it does not grow for
ever with anchors, sweep markers and management events: before the
conversation chains, the sweep cuts the chain's *prefix* written before the
cutoff (`agent_audit::cut_agent_chain`), in one transaction with a signed
`chain_checkpoint` appended at its head — `base_seq` and `base_hash` (the
last removed event), `removed`, `seqs` (the removed range), `from`/`to`
(when they were written), `before`, and `anchors`: the latest anchor of
every conversation among them that was not swept, so a live conversation
stays guarded. Anchors of swept conversations go with the prefix.
`verify` starts the agent chain at the newest checkpoint (after
`base_seq`, expecting `base_hash`) and seeds its anchors from it; a
checkpoint met further along adds its carried anchors without replacing a
newer one. Removing more of the prefix shows as a missing event, and
removing the checkpoint as a gap in the chain.

**Access and API** (`aiplane-api::pages::json_agent_activity`): the
agent-management permission plus a `read` share (admins hold one on every
agent) — the log holds whole conversations, so a responder, who answers
handoffs without a share, cannot read it (`403`).

| Method | Path | Purpose |
|---|---|---|
| GET | `/api/v0/agents/{id}/activity?conversation=&kind=&from=&to=&cursor=&order=&limit=` | A page of events (`limit` 1–500, default 100, and at most ~4 MiB of detail), newest first or `order=asc`; `kind` is a comma list; `from`/`to` RFC 3339 or `YYYY-MM-DD`; `{events, next_cursor, order}`. A conversation's events include its sub-agent runs |
| GET | `/api/v0/agents/{id}/activity/export?conversation=&kind=&from=&to=` | Every matching event, oldest first, one JSON object per line (`application/x-ndjson`), streamed a ~1 MiB batch at a time with backpressure |
| GET | `/api/v0/agents/{id}/activity/verify?full=` | From each chain's watermark, or from the start with `full=true`; `{ok, chains, events, checked, unanchored, head: {chain_key, seq, hash} \| null, broken: {chain_key, seq, event_id, reason} \| null}` — keep `head` outside the gateway to detect a log cut back to an earlier one |

An event reads `{cursor, id, kind, ts, principal_id, actor_id, agent_id,
version, conversation_id, session_id, turn_id, round, call_id, visitor_id,
caller_id, duration_ms, run_chain, detail, chain_key, seq, prev_hash,
hash}`. A sub-agent's activity shows in the log of the agent whose
conversation it ran in, not in its own.

**Server logs.** Every agent turn runs in a `tracing` span `agent_turn`
with `agent`, `principal`, `version`, `conversation`, `session`, `turn`,
`visitor`, `caller` and `depth`, and every tool call in a `tool_call` span
with `tool` and `call_id`, so a log line joins the events it belongs to.

**Storage.** A model round's request carries the whole conversation so
far, so the log does not store it whole every round
(`agent_audit::exchange`): the first round of a turn keeps its `request`;
every later round keeps a `request_delta` against the round before it —
`prev` (that exchange's event id), `keep` (how many of its messages this
request starts with), the `messages` after them, the `system` message only
when it changed, `tools` only when the offer changed (`null` when it was
dropped), and `rest` (model, stream and sampling parameters). A request that
shares nothing with the previous one beyond the system message is stored
whole. A `data:` URL of at least 4 KiB (a base64 image) is stored once per
chain in `activity_blobs` by its SHA-256, and the detail holds
`activity-blob:sha256:<hash>` instead; the reference is inside the event's
signed hash, a blob whose content no longer matches its hash is not served,
and the sweep deletes a chain's blobs with it. The activity API's page and
export serve every exchange as the whole request it stood for
(`agent_audit::Reconstructor`, which follows `prev` back to a whole request
and puts the blobs back); `activity::a_whole_run_is_one_hash_chain…` checks
that what it gives back is what was sent. So a turn of *r* rounds stores its
prompt about once plus what each round added, plus every tool result once.
SQLite stores large rows on overflow pages, which the hourly
sweep frees whole chain by chain; the file does not shrink without a
`VACUUM`, but freed pages are reused. The columns #111 added sit after
`detail` in the row, and reading a column behind an overflowing `detail`
means walking its overflow chain, so every query that does not need the
payload is answered from an index alone (a test checks the plans).
Indexes: `(chain_key, seq)` unique — chain order, the fork guard and
verification; `(chain_key, seq, hash)` — the chain head every append
reads; `(agent_id)` and `(conversation_id)` — the API pages by rowid
within either, which those single-column indexes are ordered by;
`(agent_id, chain_key, conversation_id, created_at)` — the retention
sweep; `(principal_id, kind, created_at)` from #100 — the decision trail
and analytics.

**UI.** An *Activity* tab on `/agents/{id}` ([`ui.md`](ui.md#agent-builder)).

**Tests.** `aiplane-agents`' `db/agent_audit/tests.rs` (correlation
columns, management events on the caller's transaction, every way a chain
breaks, 40 parallel writers to one conversation, paging and the byte cap,
the sweep taking whole chains only); `agents/run/tests/activity.rs` (the
whole story above, a changed event found by `verify`, six parallel
conversations each with its complete sequence, a run whose log cannot be
written stopped before the model is asked, a lost `tool_result` stopping
the next round); `agents/retention.rs` (the sweep and its marker);
`spec.rs` (audit retention at least conversation retention);
`tests/it/agent_activity.rs` (the API: timeline, filters, cursor, export,
verify, who may read it); `web/src/lib/agent-activity.test.ts`.

### What #115 built

**Problem.** An owner wrote "you do not answer questions outside your scope
and deny them politely" with no scope defined; asked about a diesel engine,
the agent answered in detail. A rule in the instructions is a request the
model may ignore, and without topics it cannot even tell what is out of
scope. The system message also named the agent by its slug
(`` You are the agent `website` ``) and ran orchestration and response
instructions together.

**Spec.** `scope: { topics, refusal, strict, classifier_model }`
(`spec::model::Scope`), all optional. The validator's rules are under
"Checks when a spec is validated" above.

**Structured system prompt.** Every agent run's system message opens with
the owner's brief (`profile::Brief`), each section only when it has
content:

```
## Role
You are <profile.display, else the principal's display name>. The sections below are your owner's instructions.

## Task
<main.instructions.orchestration>

## Scope
You cover only these topics:
- <topic>
If asked about anything else, reply exactly: <refusal>

## Tone
<main.instructions.response>
```

The slot view, the route summary and a compaction summary follow, each
after a `---` as before. `## Scope` is written whether or not the scope is
strict: without `strict` it is guidance and nothing else.

**Topic guard** (`agents::topic_guard`). Under `strict`, a main agent's turn
asks the guard before its first model call (`TurnPolicy::guard_topic`, the
one place, in `run_one_turn`; never on a resume and never for a routed
sub-agent, whose input is a task, not a visitor's message).
- *Input:* the topics, the latest visitor message and the exchange before
  it (the previous visitor message and answer), each clipped to 2000
  characters. The exchange is what lets "and what about the price?" after an
  in-scope question stay in scope; older history is left out to keep the
  call small. Greetings, thanks and "what can you do?" count as in scope.
- *Call:* `agents::model_call::ModelCall`, the mechanism the route
  classifier uses too: one non-streaming request to `classifier_model` (else
  the main model) under the principal's model grant, `response_format` an enum
  of `in_scope` / `out_of_scope`, and the answer checked again in code. It
  is a usage row of the run (so it counts against `publish.budget` and the
  model's limits), its tokens count against the turn's `main.budget.tokens`,
  and it is an `llm_exchange` with `purpose: scope_guard`.
- *Verdict:* recorded as `scope_decision`. Only a clean `in_scope` lets the
  turn continue (trust rule 1: the model may only deny). `out_of_scope`
  makes the owner's `refusal`, verbatim and untranslated, the turn's answer;
  the main model is not called. The answer still passes the output filter
  like any other.
- *Chosen: fail closed.* When the guard cannot decide (no model, a non-2xx,
  an answer that is not a verdict), the visitor gets the refusal and the
  `scope_decision` carries `verdict: failed` with the `error`. A strict scope
  promises that off-topic messages never reach the main model; failing open
  would break that promise exactly when the guard model is down and nobody is
  watching. The cost is an in-scope visitor refused during an outage, which
  the error in the log makes visible.

**Test chat.** The debug payload carries `scope: {verdict, topics, error?}`
from the turn's `scope_decision`, and the DebugPanel shows it.

**Tests.** `agents/run/tests/topic_guard.rs` (the diesel question refused
with a scripted guard upstream and metered, an in-scope question under the
structured prompt, a follow-up judged with its exchange, a non-strict scope
as prompt guidance only, a failing guard refusing and recording why, no
guard for a sub-agent); `agents/topic_guard.rs` (which specs get a guard,
what the guard is shown); `agents/profile.rs` (the brief's sections);
`spec.rs` (scope validation); `tests/it/agent_test_chat.rs` (the 422 for a
strict scope without a refusal, the verdict in the debug view).

### What #116 built

The setup assistant and overview in the SPA ([`ui.md`](ui.md#agent-setup)).
The spec is unchanged: every step reads and writes the parts of it described
in that table, and leaves the rest alone.

- **Model.** One model picker over the models the manager may grant
  ([Models](#models)), with "Default (<model>)" for the gateway's default
  chat model. #116 first shipped admin-mapped Fast/Balanced/Thorough choices
  (`agents.pool_*` settings, `tiers` on `agent-resources`); they were removed
  when agents moved from pools to models. Choosing a model grants it through
  the ordinary grant route, so the manager's cap applies unchanged.
- **Templates.** `web/src/lib/agent-templates.json`: five starter drafts
  (FAQ, customer support with an e-mail-code identity check and a fallback to
  a person, lead qualification, internal helper, blank). A string `@<key>` is
  a catalog message. `tests/it/agent_test_chat.rs`
  (`every_setup_template_is_a_valid_draft_in_every_language`) creates every
  template in all six languages through `POST /api/v0/agents`, so the
  validator accepts each as a draft.
- **Slot `order`.** A slot may carry `order` (a whole number ≥ 0, checked
  like `max_length`; `Slot::order` in the typed spec). The details step writes
  each row's position there and sorts by it, because a JSON object's key
  order does not survive a save (`serde_json` without `preserve_order` keeps
  object keys sorted, and turning that feature on workspace-wide would change
  every `to_string()` of a map — `agent_tests::spec_hash` among them — so it
  was not). Slots without one follow, by name. The run ignores it.
- **Reserved names the assistant owns.** The verifier `identity`, the slots
  `verified`, `topic` and `request`, and the route `fallback`. A route is the
  assistant's when it has exactly the hand-off shape; anything else under
  those names, and every other route, slot or verifier, is shown as "set up in
  the advanced editor" and kept as it is.

### What #117 built

The prompt assistant: from a manager's scenario (typed or spoken) or a
template, one model call proposes a value for every setup step plus test
cases; another improves one text. The UI (#116) shows the proposal per step
and applies what the manager accepts.

**Shared mechanisms.**
- *Used:* `model_call` for the constrained call, the typed spec's validator
  (`spec::validate`, on the draft as it would be), `eval::parse_case` for the
  proposed tests, the grant cap's predicates (`json_agent_resources::grantable_tools`,
  the same list `GET /api/v0/agent-resources` serves), the rate primitive
  (`rates::record_now`, new scope `manager`), the spend limits
  (`Enforcer::check_for_model`), `read_json_capped`, and the activity log's
  runtime door (`agents::audit::record_event`).
- *Introduced:* `model_call::ask_json`, the one structured (`json_schema`,
  strict) non-streaming call on a model, which `ModelCall` (route classifier,
  topic guard) and the evaluation judge now run on too; whose usage row it is stays the caller's
  (`JsonExchange::usage_record`). It sends `chat_template_kwargs.enable_thinking:
  false`, as the title and compaction calls do: a reasoning model (Qwen on
  SGLang) asked for JSON otherwise spends the whole answer thinking now and
  then and returns empty content.

**It writes nothing.** Neither endpoint touches the draft, the grants, the
versions or the test cases. A scenario that talks the model into proposing
anything at all — "publish this", "grant run_in_sandbox", "hand everything to
agent X" — yields at most an offer the manager sees, filtered as below; the
UI applies an accepted step through the ordinary draft, grant and test
routes, which check it again (the grant route with its cap). The scenario is
sent as a JSON field of the user message, never as instructions, and the
system message says to ignore instructions in it.

**API** (`aiplane-api::pages::json_agent_assist`; `can_manage_agents` and a
`write` share, admins hold one):

| Method | Path | Body | Answer |
|---|---|---|---|
| POST | `/api/v0/agents/{id}/assist/suggest` | `{scenario, template?, current_draft?, model?}` | `{steps, dropped, model, usage}` |
| POST | `/api/v0/agents/{id}/assist/improve` | `{field: task\|tone\|refusal, text, model?}` | `{field, suggestion, why, model, usage}` |

`steps` (each `null` or empty when nothing is offered):
- `task: {orchestration}` — `main.instructions.orchestration`;
- `tone: {chips, language, response}` — `main.instructions.response` as the
  task-and-tone step holds it: `chips` are the step's tone ids (`friendly`,
  `factual`, `casual`, `brief`, `detailed`, `formal`, `informal`; the schema
  enumerates them and the review drops any other word), `language` is
  `visitor`, an answer-language code or `null`, and `response` is only the
  further text no chip or language stands for. The lines each chip and
  language stand for live in `assist::tone` (a port of `TONE_LINES` /
  `responseText`, pinned against `agent-setup.ts` by a test), so an
  architect's tone is written exactly as the step reads it back;
- `scope: {topics, refusal, strict}` — `scope` (an existing `classifier_model`
  is kept);
- `abilities: [{id, name, why}]` — tools for `main.tools`, which the UI grants
  when applied; `name` is the title the ability card shows. Never
  `rag_search` / `rag_list_collections`: knowledge search without a
  collection finds nothing, so it is offered as knowledge (dropped with a
  reason if the model names it anyway);
- `knowledge: [{id, name, why}]` — knowledge bases (RAG collections) the
  manager may read and so grant, chosen by name and the admin's description
  (the model is told a base about another product does not fit, and to name
  the subject under `missing_knowledge` when in doubt); applying one does what its
  knowledge card does: the collection and `rag_search` granted, the search
  bound to it as a constant when it is the only one, `rag_list_collections`
  added when there are several (`review::set_knowledge`, a port of
  `setKnowledge`). Dropped when the manager may not grant `rag_search`;
- `missing_knowledge: [subject]` — what the agent must know about that none
  of those knowledge bases covers; the UI says an admin has to add one;
- `slots: [{name, label, type, def}]` — `state.<name> = def`. `type` is a
  friendly kind: `text` (string, ≤200, the setup's shape), `long_text` (string, ≤2000), `email`,
  `number`, `whole_number` (integer), `yes_no` (boolean), `choice` (enum of
  the proposed values); `set_by: [llm]`, `description` = `label`;
- `identity: {method, why}` — `none`, `website_login`, `email_code` or
  `lookup`; a recommendation only, since each needs a connector or a key the
  identity step asks for;
- `handoffs: [{name, topic, target, target_name, details, identity, route}]`
  — a rule of the hand-off step, `routes.<name> = route` in the shape
  `assist::handoffs::write` gives it. The model picks a `condition`:
  `always`, `details` (wait until every slot of the details step is set —
  a qualified lead) or `identity`. A rule about a topic the draft already
  hands off is left out. `identity` on a draft with no identity check is
  kept when the same offer recommends one (other than `none`): applying the
  hand-offs sets up that check first (`applySuggestedRules`), so the rule
  is written gated — `route` carries no identity leaf until then. With no
  check recommended either, the condition is left out with a note;
- `tests: [{name, kind, script, expect}]` — the body `POST …/tests` takes, at
  most 6. Every case expects `finished: true`; an `out_of_scope` case expects
  the draft's refusal in the answer (and is dropped when there is none yet).

`dropped: [{step, item?, reason}]` lists what was left out and why, in words.

**What may be offered.** The model is given — and its schema enumerates —
exactly the tools this manager holds and may grant (ids with friendly names,
knowledge search aside), the knowledge bases they may read (by name;
`json_agent_resources::grantable_collections`, the list `GET
/api/v0/agent-resources` serves) and the agents shared with them (not the
agent itself) plus `human` as targets. Because a backend may ignore the schema, the review checks again: an
ability or a target outside those lists is dropped, never offered.

**Validation per piece.** Pieces are applied in step order to the draft the
manager is editing (`current_draft`, else the stored draft), each checked with
`spec::validate` at the `Draft` stage against the agent's grants as stored now
plus the tools and collections offered so far (applying an ability or a
knowledge base grants it). A piece is kept when it adds no issue the draft did not already have —
so a draft that is invalid already does not sink the proposal — and dropped
with the issues it would add. Later pieces build on kept earlier ones: a
hand-off waiting for the details gates on the slots that were kept. A slot or
route name the draft already has is kept as it is, not replaced. Names are
made spec identifiers (`Order Number` → `order_number`). A step whose JSON
does not read is dropped whole; the others stand.

**Model.** The request's `model` when it is one of the chat models the
manager may use (`403 assist_model_not_allowed` otherwise), else the draft's
`main.model` when they may use it, else the first of their chat models — the
gateway's default chat model when they may use it (`assist::choose_model`
over `server::model_choices::offered`, [Models](#models)); `503
assist_no_model` when they may use none. The manager's access decides, not
the agent's grants: it is the manager's call. An automatic-route alias is
resolved by its selector, as in the chat.

**Recording.** *Chosen:* a manager's call, not an agent run.
- A usage row of the manager's (`principal_kind = user`, `source = chat`, like
  the rest of the session UI), under the manager's own spend limits
  (`429 rate_limit_exceeded` with `Retry-After` when over).
- In no conversation chain. The agent's own chain gets one `assist_suggested`
  event per call, actor = the manager: `action` (`suggest`/`improve`), the
  `scenario` and `template` (or the improved `field` and `text`),
  `model`, `usage` (token counts), `latency_ms`, `offered` (the step kinds),
  `dropped`, and `error` for a failed call. *Chosen to keep the scenario:* it
  is the manager's own input, and without it the event cannot explain what
  was proposed. The proposal itself is not kept (it is in the answer, and
  what is applied is recorded by the routes that apply it). The event is
  written for a failed model call too; when it cannot be written the call
  answers `503 activity_log_unavailable`.

**Limits.** `ASSIST_RATE` = 30 calls per manager per agent per hour, suggest
and improve together, on the agent's `rate_events` (counter
`assist:<user id>`); `429 assist_rate_limited` with `Retry-After`. The rate is
per agent because `rate_events` is keyed by the principal; the spend limits
bound a manager across agents. Scenario and improved text at most 8000
characters, template at most 200, body at most 512 KiB (`400
invalid_assist_input`, 413). Model timeouts 120 s (suggest) and 60 s
(improve); a failed or unreadable answer is `502 assist_model_failed`.

**Tests.** `agents/assist/review/tests.rs` (a good proposal applied to the
draft passes `spec::check` at draft and publish stage; an ungrantable ability
never offered; tone chips only by the step's ids; knowledge by knowledge
base, never as the search tools, and wired like its card; an invalid slot
dropped with the validator's reason and a details hand-off gating on the
slots that were kept; targets limited to shared agents and people; unreadable steps dropped whole; test-case rules; existing names kept;
the schema's enums); `tests/it/agent_assist.rs` (the endpoint end to end on a
scripted model: checked steps, every offered test case accepted by the tests
route, nothing written, the event and the usage row; knowledge bases offered
by name and refused without knowledge search; a prompt-injection
scenario; improve; the rate refusing with `Retry-After` and per manager; bad
input and missing shares refused before the model is asked; a failed model
call recorded and answered `502`).

### What #118 built

The agent architect: a conversation in which a built-in assistant plans an
agent with a manager — asks questions, proposes a structure, explains
trade-offs — and creates or changes the draft as it goes. The UI is in
[`ui.md`](ui.md#agent-architect).

**Chosen: a persona of the person's chat, not an agent.** The architect is
not a system principal and holds no grants. Its conversation is an ordinary
chat of the signed-in person's (`chat_sessions.user_id`), marked in
`agent_architect_sessions` (session, person, the agent it plans), and every
turn runs on the person chat driver as `TurnPolicy::Persona`
(`aiplane_runtime::persona::ChatPersona`): the person's models, budget, usage
and history, but the architect's fixed system prompt and only its tools,
offered every round. The person's own chat tools are neither offered nor run
(a call to one is refused as an unknown tool). The prompt is English and
tells the model to answer in the person's language.

**Tools** (`aiplane-api::pages::architect::tools`). Each acts as the person
through the function the matching route uses, so the share, the validator
and the grant cap decide exactly as for a click in the setup UI, and a
refusal reaches the model — and the chat — as the route's own message and
code. Each checks `can_manage_agents` again when it runs.

| Tool | Does | Through |
|---|---|---|
| `list_agents` | the agents shared with the person | `visible_agents` (as `GET /api/v0/agents`) |
| `read_agent(agent_id)` | draft, grants, what blocks publishing | `agent_by_id` (`read` share), `SpecWorld`, `publish_issues` |
| `list_grantable` | models by kind, the gateway's default models, tools, connectors, skills, collections | `resources_for` (as `GET /api/v0/agent-resources`) |
| `propose_setup(agent_id, scenario, template?)` | the #117 proposal; writes nothing | `suggest_for` (as `…/assist/suggest`: its rate, usage row and `assist_suggested` event) |
| `create_agent_draft(display, id?, description?)` | a new agent, unpublished; the id is derived from the name like the create dialog's `agentIdFromName`; its `main.model` is left unset, so it runs on the gateway's default chat model, which is granted (capped) when the person may grant it; the answer names it as `model` | `create_agent` (as `POST /api/v0/agents`) |
| `update_agent_draft(agent_id, changes)` | changes the draft step by step | `assist::apply_changes`, then `add_capped_grant` per needed grant, then `save_draft` |
| `run_test_turn(agent_id, message, conversation_id?)` | one test-chat turn of the draft | `draft_test_turn` (as `…/test-turn`) |

There is **no publish tool**: the prompt tells the model to send the person
to the setup page (`setup_url`), where Publish stays a click. An agent may
be named by its id or its name.

**`changes`** (`assist::changes_schema`): `display`, `model`, `task`, `tone`,
`scope`, `abilities` and `slots` in the prompt assistant's step shape, plus
`knowledge: [{name, why}]`, `handoffs: [{topic, target, details?,
identity?}]` and `fallback_to_person`.
`apply_changes` runs the same `Reviewer` as `review`: each piece is applied
to the stored draft and kept when it adds no validator issue, else dropped
with the reason. An ability is offered only when it is one of the person's
grantable tools, and `model` only when it is a chat model the person may grant —
both are then granted through the capped grant route before the draft is
saved, so a grant the person lacks is never made (`grant_exceeds_manager`
would refuse it anyway). When nothing at all is applied the call fails with
the reasons, so the chat shows it as an error.

*Written in the setup's shapes.* What the architect writes must read in the
setup assistant as what it is, not as "set up in the advanced editor":
- a slot gets the setup's friendly-kind shape (`text` is ≤ 200, as
  `SLOT_SHAPES`) and the next `order`; a hand-off waiting for every detail
  is regated on the new set (`handoffs::with_details`, the port of
  `withDetails`, which the details step and the identity step's added slots
  go through as well);
- hand-offs are the hand-off step's rules: `assist::handoffs` is a port of
  `readHandoffs` / `writeHandoffs` / `deriveBind` from
  `web/src/lib/agent-setup.ts` (routes `when: {all: [topic eq, request set,
  (one `set` leaf per detail slot — read as a rule only when the leaves
  name exactly the detail slots, in any order; a route on some of them is
  custom and kept verbatim), (verified provenance)]}`, `task`
  `Request about {topic}: {request}`, the
  `topic` enum and `request` slots, `router.order`, the `fallback` route). A
  rule whose topic exists replaces it; other routes stay as they are. The
  bind comes from the specialist's live spec; `identity` is honoured only
  when the agent has an identity check (an architect cannot set one up, so
  unlike the prompt assistant's offer a recommended method does not count), `details` only when it collects
  details (otherwise kept without it, with a note). `web/src/lib/fixtures/architect-draft.json` is a draft written by
  `apply_changes` (pinned in `review/tests.rs`) that `agent-setup.test.ts`
  reads back as rules and friendly slots, and writes back unchanged.

Identity checks need a connector or a key only the setup's identity step
asks for, and test cases are saved from its last step; both are left to the
UI.

**Undo: draft revisions.** Every draft save (the UI's and the architect's)
keeps the draft it replaced in `agent_draft_revisions` (newest
`MAX_DRAFT_REVISIONS` = 50 per agent; an unchanged save keeps none), and
answers with that `revision`. `update_agent_draft` returns it, and the
window's *Undo* calls `POST /api/v0/agents/{id}/draft/restore {revision}`,
which validates and saves it like any draft (so the undo is itself
undoable). A revision also keeps the grants its change made
(`agent_draft_revisions.granted`, filled by the architect's
`update_agent_draft` and `create_agent_draft`). The undo revokes those
after the save unless the restored draft or the live version still uses one
— "uses" is asked of the validator (`uses_grant`: the spec has more issues
without the grant), not a list of fields. The activity log has the draft
event with `restored` and `revoking`, and a `grant_removed` per revocation;
the answer lists them as `revoked`.

**API.**

| Method | Path | Body | Answer |
|---|---|---|---|
| POST | `/api/v0/agent-architect` | `{agent_id?, title, fresh?}` | `{session_id, model, agent_id, resumed}` |
| POST | `/api/v0/agents/{id}/draft/restore` | `{revision}` | `{draft_spec, live_version, revision, revoked}` |

`agent-architect` needs `can_manage_agents` (403 otherwise) and a `write`
share on `agent_id`. It reopens the person's newest architect conversation
about that agent (or about no agent yet) unless `fresh`; otherwise it
creates one titled `title` (the client sends "Agent architect: <name>" in the
person's language). `model` is what to send messages with: the prompt
assistant's choice (`assist::choose_model`: the gateway's default chat model
when the person may use it, else their first chat model), `503
architect_no_model` when they may use none. Messages then go through the
ordinary `POST /api/v0/chat/sessions/{id}/messages` and stream on its events
endpoint; `spawn_assistant_worker` looks the session up and builds the
persona for it.

**Chosen: the conversations stay in the chat history.** They are the
person's chats, titled "Agent architect: …", and are not hidden from the
chat list: hiding would need a filter in the session list and the sidebar
for no safety gain, and opened from `/chat` they still run as the architect,
since the persona belongs to the session, not to the page.

**Tests.** `agents/assist/review/tests.rs` (changes applied with their
grants; a model or tool the person may not grant dropped);
`openai_driver/resume.rs` (`a_personas_turn_offers_only_its_tools_and_refuses_the_persons`);
`persona.rs`; `db/agents.rs` (revisions kept, capped, unchanged saves
skipped); `db/architect_sessions.rs`; `pages/architect/tools.rs` (the id from
a name, no publish tool); `tests/it/agent_architect.rs` (a scripted
conversation lists, creates, proposes, updates, is refused an ability the
person lacks, tests the draft and cannot publish, every call recorded; the
offered tools; undo through `draft/restore`; reopening and `fresh`; 403 for a
non-manager; an undo revoking the change's tool but keeping the model the
restored draft runs on, logged; an undo keeping a grant the live version
uses); `web/src/lib/architect.test.ts`.

### What #119 built

Voice in the embed widget: a visitor may speak a message and hear answers.

**Shared mechanisms.**
- *Used:* the typed `AgentSpec`, the embed `visitor()` chain and `admit`
  (rates and owner budget), `read_body_capped` / `read_json_capped` under
  `BodyLimitLayer::HANDLER_CAPPED`, `PoolAccess::for_system_models`, the VAD
  (`aiplane_features::server::vad`, moved down from the gateway crate so the
  API layer can trim a recording too), `speech::to_spoken`, `UsageRecord::in_run`,
  `agents::audit::{RunLog, SideExchange, anchor}`.
- *Introduced:* `aiplane-runtime::agents::voice` — the two calls, made as the
  agent on the one model the direction runs on; `RunLog::visitor` for an event of a
  visitor's conversation between turns; `web/shared/wav.ts` (the WAV encoder
  the SPA composer and the widget now share) and `web/shared/color.ts`.

**Spec.** `publish.voice: { input, output, voice?, transcription_model?,
speech_model? }` (`spec::model::VoiceSpec`). Both directions are off by
default. A model named here must be granted to the agent. A direction that
is on and names none runs on the gateway's default transcription or speech
model ([Models](#models)), which must then be granted; publishing a direction
without either is refused (`422`, at `publish.voice.transcription_model` /
`speech_model`, naming the setup's *Website* step). Either way an agent
reaches only models granted to it, and the embed endpoints answer `503
voice_unavailable` when the grant is gone. `voice` is the TTS voice; unset,
the voice the pool of the backend that synthesises it maps the visitor's
language to (`Acquired::voice_for`) applies. `VoiceSpec::{input_model,
output_model}` return the named model only for a direction that is on. `profile.color`
is now checked as `#rrggbb` (`Profile::color()`), and the widget paints
itself in it.

**Endpoints** (`aiplane-api::pages::embed::voice`, visitor token, origin
chain as for messages):

| Method | Path | |
|---|---|---|
| POST | `/api/v0/embed/agent` | `{key}` → `{agent: {display, color, voice: {input, output}}}` before any conversation; not rate-gated (reads cost nothing). `start`/`session` return the same `agent` |
| POST | `/api/v0/embed/transcribe` | body `audio/wav`, 16 kHz mono 16-bit PCM, at most **2 MiB** (`413 payload_too_large`), 0.4–60 s (`400 audio_too_short`, `413 audio_too_long`), anything else `415 unsupported_audio` → `{text}` (at most 8 000 characters). Not posted to the conversation: the widget puts it in the input for the visitor to read, change and send |
| POST | `/api/v0/embed/speak` | `{turn_id}` → `audio/mpeg` (`204` when nothing is speakable). Only a `completed` assistant turn of *this* visitor's conversation that no worker holds any more (`404 turn_not_found`, `409 turn_not_final`): the content as stored after the output filter ruled, never text from the client; Markdown is turned into speakable prose, and at most 3 000 characters (to the last whole sentence) are spoken |
| GET | `/api/v0/embed/recorder.js` | the audio worklet the widget records with — served under the embed CORS a cross-origin worklet needs |

A direction that is off answers `404 voice_not_enabled`; a failing backend
`503 voice_unavailable` (the real error is in the activity log and the server
log). Both calls go through `admit` first, so they count against the
visitor's and the IP's rate like a message and are refused once the owner's
budget is spent. A spoken turn is cached in memory per turn, model and voice
(256 entries, 64 MiB at most), so replaying costs no second synthesis; a
replay still counts against the rate.

**Usage and log.** Each call that reaches a backend is a usage row of the
agent run (`kind` `transcription` with the recording's seconds, or `speech`
with the characters spoken; `agent_id` set, so it spends the owner's
budget) and an `llm_exchange` in the conversation's chain with `purpose:
transcription` or `speech`, the visitor on the event and, for speech, the
`turn_id`. The transcription event keeps the request without the audio
(`file: {content_type, bytes, seconds}`) and the transcript; the speech
event keeps the text sent and `{content_type, bytes}` of the audio. The
chain is anchored after each call. **Audio is never stored**: a recording
lives in memory for the request, and spoken audio only in the bounded cache.

**Widget** (`web/embed/voice.ts`, `audio.ts`, `theme.ts`). A microphone
button when `voice.input`: held down it records until release, a short
click starts a recording the next click (or Enter/Space) sends; a
recording stops by itself at 60 s; Escape or *Cancel* throws it away; a
refused permission, a missing microphone or an insecure page each get their
own message. A speaker toggle when `voice.output`: off until the visitor
switches it on (that click unlocks audio, so nothing ever autoplays), then
every answer that finishes is read aloud; *Stop* ends it. Playback decodes
into Web Audio, so no `blob:` URL is needed. Animations stop under
`prefers-reduced-motion`. The recorder logic is a pure state machine
(`micStep`) with unit tests.

**Builder.** The setup's *Website* step binds the colour and `publish.voice`
(`readColor`/`writeColor`, `readVoice`/`writeVoice` in `agent-setup.ts`);
each direction has a model picker over `models.transcription` /
`models.speech` of `GET /api/v0/agent-resources`, with "Default (<model>)"
for the gateway's default; choosing one stages its grant. Switching a
direction on starts it on the default (its grant staged); a direction the
manager may grant no model for is explained instead of offered.

**Tests.** `tests/it/embed/voice.rs` (transcript returned and not sent, no
audio in the log, disabled → 404, body cap with a finite oversize and
"endless" body, length and format, visitor rate, owner budget, only a final
answer of this visitor spoken from the stored text, cache, owner's voice,
`describe`, worklet, voice models); `spec.rs` and `spec/model.rs`;
`web/embed/{voice,theme,api}.test.ts`, `web/shared/{wav,color}.test.ts`,
`web/src/lib/agent-setup.test.ts`.

### Models

An agent names models the way a person picks one in the chat: a model id, a
backend alias or an automatic-route alias. Every pool reference the builder
used to have is a model key now: `main.model`, `router.model`,
`scope.classifier_model`, `publish.voice.transcription_model` /
`speech_model`. A sub-agent runs on its own spec's `main.model`; the
evaluation judge on the agent's main model. There are no agent-specific model
settings (the Fast/Balanced/Thorough tiers of #116 and their
`agents.pool_*` settings are gone).

**One list** (`aiplane-runtime::server::model_choices`). `offered(state,
kind, access)` is what a caller with `access` may pick of a kind (chat,
transcription, speech): the registry's models and aliases of that kind
(`models_with_compliance_for_kind_for`, then the access's model check), plus
— for chat — every automatic route whose alias the caller may name and whose
fallback they may reach, sorted, the gateway default promoted to the front.
`GET /api/v0/models` (the chat picker), `GET /api/v0/transcription_models`,
`GET /api/v0/agent-resources`, the grant cap (`grant_holding::holds` for kind
`model`) and the prompt assistant's choice all read it. It sits in
`aiplane-runtime` because each consumer is there or above and it needs the
automatic routes and the feature defaults beside the registry. An automatic
route carries `RouteChoice { candidates, whole }`: `whole` when the caller
may also use every candidate and the selector. The chat picker lists a route
as before (its fallback reachable); a manager may **grant** one only when it
is whole (`ModelChoice::grantable`), because the grant hands the agent every
model the route can send to.

**Unset means the gateway default.** `gateway_default(state, feature)`:
the admin's *Default models* choice (`/admin/models`, `app_settings`
`default_model.{chat,transcription,speech}`) when the gateway offers it, else
the first model it offers — `feature_defaults::resolve`, over the whole
gateway, not over the agent's grants. `agents::defaults::{main_model,
voice_model}` apply it at run time; `SpecContext::model_defaults`
(`defaults::model_defaults`) carries it to the validator, which on publish
requires the default an unset key runs on to exist and be granted. So an
admin changing the default moves every agent that names none — and one not
granted the new default stops with `ModelNotGranted` (`422
agent_model_not_granted` in the test chat; voice answers `503`) until it is
granted or the agent names a model.

**Pools of a grant.** A `model` grant stores, in `principal_grants.pools`,
every pool of the model's kind the granting manager may use — for an
automatic route, of chat and selector models (`grant_holding::model_grant_pools`,
`UpstreamRegistry::pools_of_kinds`). That is decided by the manager's
groups, not by what is serving at grant time: a pool that is down or not
probed yet still counts once it serves the model; routing takes the recorded
pools that serve it at request time. The principal routes the name only
through those pools, as a person's chat stays inside their groups' pools. An
admin's grant stores `NULL`: every pool serving it, as an admin's own
requests reach. **A regrant never narrows**: the pools become the union of
what the grant held and the new grantor's pools (`NULL` wins), the first
grantor stays, and `POST …/grants` answers `{added, widened}` (`201` when
new, `200` otherwise, `widened` when it now reaches more pools). Taking a
pool away is a revoke and a new grant. The grant persists like every other.

**A token narrows it again.** A `gws_` token minted by a non-admin carries
each model grant only through the pools of the model's kind its minter may
use *now*, intersected with the grant's (`grant_holding::capped_to_minter`,
cached with the narrowed pools); an empty intersection drops the grant. An
admin's token keeps the grant's pools.

**Access** (`PoolAccess::granted_models`). A system principal's access is its
`model` grants, each with its pools: the grant replaces the pools' group
rule, and a name routes only through its pools (`PoolAccess::reaches`).
A granted backend alias authorises what a backend of one of its pools
resolves it to, there and nowhere else — but only for the gateway's own
resolution of a name the caller was allowed (`PoolAccess::resolving` /
`for_request`): the turn resolves `fast` to the real id before it routes,
the `/v1` chat and messages paths resolve the requested name up front, and
the conversation's compaction routes the resolved id too. A caller naming
the target itself (`Qwen/Qwen3` with only `fast` granted) holds no grant on
it: `404`, and `GET /v1/models/{id}` does not know it. An agent run
narrows that to the one model it uses (`PoolAccess::for_system_models`), and
so do the topic guard, the route classifier, the evaluation judge and voice.
Default deny: no `model` grant, no model. A granted automatic route is
resolved like a chat turn (`server::model_route::route_target` →
`AutomaticRouter::select`): the selector and candidates are reached under
the access widened by the route's members (`PoolAccess::for_route_targets`
over `AutomaticRoute::members`, each through the route grant's pools), and
the chosen target is routed under the access widened by exactly that
target; a candidate that is itself an alias reaches its target the same
way. Compaction compacts on the target the route pinned for the session,
else its fallback (`compaction::compaction_target`). The kind's unknown-model
fallback (`[fallback].<kind>`) only applies to an agent when it is granted
as well. A person's token allowlist still stops
at the alias, as before.

**Setup.** One picker per model key (`ModelPicker.svelte` over
`SearchableSelect` and `modelSelectOptions`, the chat picker's pieces), with
"Default (<model>)" from `defaults.<kind>` of `GET /api/v0/agent-resources`
when the spec leaves the key unset. Choosing a model stages its grant;
choosing Default stages the default's grant when the manager may give it.

Tests: `server::model_choices` (an auto-route with an unusable candidate
offered but not grantable, per-kind lists with the default first, an agent
offered exactly its grants), `upstreams::registry`
(`a_system_principal_reaches_only_its_granted_models_whatever_the_pool_groups`,
`an_agent_run_reaches_only_models_both_granted_and_listed`,
`an_automatic_route_target_widens_a_principal_by_exactly_the_target`),
`spec.rs` (`an_unset_model_runs_on_the_gateway_default_which_must_be_granted`,
`a_published_voice_direction_runs_on_a_granted_model`), `agents/run/tests.rs`
(`an_agent_run_uses_only_the_model_its_spec_names`,
`an_agent_without_a_grant_on_its_model_does_not_run`,
`an_agent_without_a_model_runs_on_the_gateway_default`),
`tests/it/automatic_routing.rs`, `tests/it/system_principals.rs` (the grant
cap for models and automatic routes), `tests/it/embed/voice.rs`,
`web/src/lib/agent-setup.test.ts` (picker options, spec mapping).

## 6. Crate placement

The rule from `AGENTS.md`: put code as high as it will go, and never reference
upward.

| Piece | Crate | Why there |
|---|---|---|
| Migrations (one embedded set, agent tables included); the `can_manage_agents` resolver check; `Principal`, `GrantSet`, `RunChain`; the `agent_id` column of usage and the per-agent spend limits | `aiplane-core` | the migration history is never split; identity types are read by RBAC, the upstream registry and usage metering, all below the features |
| db modules for `system_principals`, `principal_grants`, `system_tokens`, `agents`, `agent_versions`, `agent_shares`, `agent_embed_keys`, `visitor_sessions`, `agent_state`, `agent_audit`, `agent_test_cases`/`runs`/`results`, `a2a_contexts`, `agent_a2a_tasks`, verifiers, analytics, responders, notify channels, retention; principal-owned conversations, the agent pause sweep and the inbox reads (`db::run_sessions`); the visitor rate gate and the one rate primitive (`rates`, over `rate_events`); the inbox webhooks (`notify_channels`) | `aiplane-agents` | *as moved (#109):* nothing below the runtime reads them, so they sit on `aiplane-core` beside `aiplane-features`; an agent DB edit no longer rebuilds the base layer, and a runtime edit does not recompile them |
| `chat_turn_suspensions` db fns; `suspended` status; new `chat_json` events (`suspended`, `state`, `gate`) | `session-core` | the chat substrate owns turn lifecycle and the SSE protocol; it reads a conversation by `user_id` and treats any other owner as opaque |
| Spec types and validation, the gate evaluator, the schema-subset validator, template rendering | `aiplane-runtime` (`agents/`) | the lowest crate that needs them at run time; `aiplane-api` validates on save through it |
| `ToolContext.principal`/`run`, `RunProfile`, finish/budget/trim/repeat/suspend, `AgentToolSource` (synthetic tools, router, dispatch, the `loop` route), output filter, tool-output injection hook, principal-aware tool/skill/MCP/model resolution; the one model list (`server::model_choices`) and the model routing a turn and an agent's own calls share (`server::model_route`) | `aiplane-runtime` | they are the loop and the tool machinery; every consumer of the model list (chat picker, agent resources, the grant cap) is here or above |
| Verifier tools (`mcp_code`, lookup), host JWT | `aiplane-runtime` (`agents/verifier/`) | *as built (#95):* they are run-scoped synthetic tools like `set_<slot>`, built from the spec and writing through `TrustedWriter`, so they sit beside them; `aiplane-tools` cannot be reached from the run |
| `/api/v0/agents/*`, `/api/v0/system-principals/*`, grants, shares, versions, embed keys, HiL inbox, the resume endpoint, the internal test chat | `aiplane-api` | JSON handlers |
| `/api/v0/embed/*` routes and CORS (`rama_server::embed_cors`), `gws_`/`gwv_` bearer dispatch | `gateway` | routing glue only; the embed CORS layer reads embed keys, so it cannot sit in `aiplane-core` beside the `/v1` one |
| The A2A client behind an `a2a` route: guard, card cache, exchange | `aiplane-runtime` (`agents::a2a_client`); the waiting task's row in `aiplane-agents` (`db::agent_a2a_tasks`) | *as built (#101):* `forward_request` dispatches it like a sub-agent, so it sits beside the router |
| The A2A agent card and JSON-RPC handlers (`/a2a/agents/*`) | `aiplane-api` (`pages::a2a`), routed in `gateway`; the spec section, card and state mapping in `aiplane-runtime` (`agents::a2a`) | protocol handlers over the same runner the embed endpoint uses |
| The prompt assistant: the structured call (`model_call::ask_json`), the proposal, its review against the draft (`agents::assist`); its handlers (`pages::json_agent_assist`) | `aiplane-runtime`; `aiplane-api` | the review needs the validator and the test-case parser, both runtime; the handlers resolve the manager's grantable tools and shared agents with the helpers `GET /api/v0/agent-resources` and `GET /api/v0/agents` use |
| The agent architect: the persona hook (`persona`, `TurnPolicy::Persona`) and `assist::apply_changes`; its tools, the start route and `draft/restore` (`pages::architect`); `agent_architect_sessions` and draft revisions (`db::architect_sessions`, `db::agents`) | `aiplane-runtime`; `aiplane-api`; `aiplane-agents` | the driver only knows "a prompt and a tool source"; the tools need the route functions (share checks, grant cap, test chat), which live in the API layer, so the API builds the persona per turn and hands it down |
| Builder UI, test chat, inbox | `web/` (SPA) | daisyUI + Tailwind, all strings through Fluent |
| Embed widget | `web/embed/`, its own Vite entry built to `target/frontend/build/embed.js` | must not pull in the SPA; strings still come from the shared catalogs |

Nothing here needs a new Cargo dependency. Host-JWT verification uses the
existing `jsonwebtoken` (now also a dependency of `aiplane-runtime`), patterns
use `regex`, and hashing uses the token helpers.

## 7. Issue map

| Issue | Sections | Scope change from this design |
|---|---|---|
| #77 non-person principals, system tokens | §1 | Separate `system_principals`, `principal_grants` and `system_tokens` tables (`gws_`); `gateway_groups.can_manage_agents`; new connector scope `agent`; usage and audit get `principal_kind` |
| #78 finish contract | §3 RunProfile, §4 validator | `finish` is a synthetic tool; payload checked by the schema-subset validator |
| #79 budgets | §3 | `Budget` in `RunProfile`; chat derives it from `Effort` |
| #80 trim tool results | §3 | as described |
| #81 repeated calls | §3 | 3 identical calls warn, the 4th ends the run as `incomplete` |
| #82 suspend/resume | §3 | `chat_turn_suspensions` in session-core; `suspended` event; nested suspensions (built for agent runs afterwards: visitor and staff resume routes) |
| #83 run identity, call chain | §1, §3 | `ToolContext.principal` replaces `user_id`/`roles`; `RunChain`; `chat_sessions` owner becomes user **or** principal (table rebuild); `agent_audit` |
| #84 agent definition | §2 | `agents` keyed by principal; `draft_spec` plus immutable `agent_versions`; shares: every share needs the permission |
| #85 typed state | §3 State | `agent_state` table; `set_by` list per slot; `subject` slot type |
| #86 gates | §4 | JSON condition tree instead of the string expressions in the epic's example; subject-bound routes must require a non-`llm` provenance |
| #87 router | §3 synthetic tools | `forward_request()` with no route argument; classifier picks only among open routes |
| #88 sub-agents | §2, §3 dispatch | a sub-agent is a separate agent (own principal, grants, versions), run as a child session |
| #89 output filter | §5 output policy | public main agents are buffered per answer |
| #90 builder UI, test chat | §2, §6 | draft runs only in the test chat |
| #91 public endpoint, visitor sessions | §5 | `gwe_` embed keys, `gwv_` visitor tokens in `sessionStorage`, fetch-streamed events, CORS only on `/api/v0/embed/*` |
| #92 limits, budget, models, retention | §1, §5 | limits subject `system` plus the spec's `publish.budget`; exact per-visitor and per-IP windows; runs narrowed to granted ∩ named models; retention sweeps agent conversations |
| #93 injection scanning | §6 | a hook on tool results inside the runner, recorded in `agent_audit` |
| #94 widget | §5, §6 | script in shadow DOM, not an iframe; own Vite entry |
| #95 verifiers | §2 `verifiers`, §5 secure input | secure input resolves a `secure_input` suspension; host JWT through `jsonwebtoken`; verifier tools in `aiplane-runtime`, not `aiplane-tools` ([built](#what-95-built)) |
| #96 human in the loop | §3 suspend/resume | builds on `chat_turn_suspensions`; `human` route kind; `request_human` is a synthetic tool in `aiplane-runtime` (it needs the run's gates), not an `aiplane-tools` tool; responders instead of a share for support staff; Slack and Discord incoming webhooks; answers in the inbox only |
| #99 evaluation | §5 | stored cases (script plus deterministic expectations), runs against the draft or a version through the test chat's door, a Goal-Plan-Action report, an optional rubric judged apart, `publish.require_passing_tests`; Tests tab |
| #100 analytics | §5 | derived from `agent_audit`, `usage_events` and the chat tables; one index, no new store; Analytics tab |
| #102 A2A server | §5 | per-agent opt-in `publish.a2a`; agent card and JSON-RPC endpoint under `/a2a/agents/{id}`, A2A v1.0; callers are `gws_` principals granted `a2a_caller` on the agent; a context is a principal-owned conversation recorded in `a2a_contexts`, a task one assistant turn ([built](#what-102-built)) |
| #101 A2A client | §3 dispatch | route target `a2a` (card URL, sealed auth, the route's own `finish` and `budget`); grant kind `a2a_agent` by card URL, admins only; resolve-and-pin SSRF guard with `$AIPLANE_ALLOW_PRIVATE_NETWORKS`; structured `input-required` is a `secure_input` pause ([built](#what-101-built)) |
| #111 activity log | §5 | `agent_audit` becomes a hash-chained activity log (per conversation, per agent); every model exchange, tool call, state write, turn and decision of an agent run recorded in full, synchronously, failing the run closed; `publish.audit_retention_days`; `/api/v0/agents/{id}/activity` (+ `export`, `verify`); Activity tab ([built](#what-111-built)) |
| #103 loop route | §3 dispatch | route target `loop` (`worker`, `critic`, `max_iterations`, `budget`); the critic's finish schema must require a boolean `accepted`; the route budget caps the sum through a shared `SpendMeter`; a pausing child is withdrawn ([built](#what-103-built)) |
| #115 topic guard, structured prompt | §2, §3 | `scope` in the spec; a strict scope's guard classifies each visitor message with a small model and answers out-of-scope ones with the refusal, failing closed; the system message in `## Role`/`## Task`/`## Scope`/`## Tone` sections ([built](#what-115-built)) |
| #116 setup assistant | §2 | overview, routed step assistant and single-step modal over the same spec; one model picker over what the manager may grant ([Models](#models); the admin-mapped tiers it first shipped are gone); five starter templates validated in six languages ([built](#what-116-built)) |
| #117 prompt assistant | §2, §5 | `POST …/assist/suggest` and `…/assist/improve`: a proposal per setup step and test cases, each piece checked against the draft and dropped with a reason; writes nothing; a usage row of the manager's and an `assist_suggested` event ([built](#what-117-built)) |
| #118 agent architect | §2, §6 | a persona of the person's chat (`TurnPolicy::Persona`), not an agent; seven tools through the routes' own functions, no publish; `apply_changes` for step-wise draft edits; draft revisions and `draft/restore` for undo; `POST /api/v0/agent-architect` ([built](#what-118-built)) |
| #119 widget voice | §5 | `publish.voice` with a granted model per direction (named, or the gateway default); `POST /api/v0/embed/{transcribe,speak,agent}`; transcript returned to the visitor, never sent for them; only a final stored answer is spoken; audio never stored; widget colour from `profile.color` ([built](#what-119-built)) |
| #97 later | — | unchanged |

## Deferred

- **Streaming on public agents.** The first release buffers each answer of a
  public main agent until the output filter has checked it. A streaming mode for
  agents without an output filter can follow once the filter exists.
