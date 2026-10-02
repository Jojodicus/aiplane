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
  That covers pools, RAG collections and MCP connectors.
- The MCP manager, memory, personal skills and per-user tool prefs are all keyed
  by `user_id`.

An agent that went through any of these paths would silently inherit
company-wide rights. That contradicts the decided default-deny.

### Decided

- Agents run as **named system principals**, never on a user's token.
- A new principal has **no rights**. Tools, connectors, skills, RAG collections
  and pools are granted one by one, and nothing company-wide is inherited.
- Only holders of the **agent-management permission** create or configure
  principals.
- A manager can only grant **what they hold themselves at grant time**. After
  that a grant persists until the principal is reconfigured, independent of the
  manager's later rights.
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
    kind         TEXT NOT NULL,              -- 'tool' | 'connector' | 'skill' | 'rag_collection' | 'pool'
    ref          TEXT NOT NULL,              -- tool id, connector key, skill name, collection id, pool name
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
  resource: `allowed_tools`, `resource_allowed` for pools, collections and
  connectors, `allowed_skills`. A refused grant returns an actionable error
  naming the resource. Grants are never re-derived later.
- **Every grant change is audited** in `agent_audit` (§3). The table landed with
  #77 (migration `0077`), with one column more than §3 lists: `actor_id`, the
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
| Pools/models (`PoolAccess`) | `allowed_groups` plus the token allowlist | `grants[pool]` only |
| Memory, personal skills, user tool prefs | yes | no access: `user_id()` is `None` |
| Usage | `usage_events.user_id` | `usage_events.principal_kind = 'system'`, with the principal id in `user_id` |
| Limits | subject `user`/`role`/`global` | subject `system`: the owner's budget for this agent (#92) |
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
  who only answer handoffs use the HiL inbox (#96), which needs no share.

**Grants are not versioned.** They belong to the principal and persist until
reconfigured, as decided. If a live spec references a tool whose grant was
revoked, it hits default-deny at run time and `on_tool_unavailable` decides what
happens (§3).

### Spec layout

Stored as JSON. The builder UI and the API speak the same shape. YAML here is
only for readability.

```yaml
profile: { display: "croit Support", avatar: null, color: null }
main:
  pool: chat-conversational             # must be in grants[pool]
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
verifiers:
  otp: { kind: mcp_code, connector: erp, send_tool: send_code, check_tool: check_code,
         input: secure_field, max_attempts: 5, code_ttl: 10m }
router: { kind: rules }                 # or { kind: classifier, pool: small-fast }
routes:
  billing:
    when: { all: [ { slot: issue, eq: billing },
                   { slot: verified, provenance: "verifier:otp", max_age: 15m } ] }
    agent: 5b1c…                        # the agent id (system_principals.id) of another agent
    task: "Invoice question from customer {verified.customer_id}: {issue_summary}"
    bind: { customer_id: verified.customer_id }
  technical:
    when: { slot: issue, eq: technical }
    agent: 9e04…
    task: "{issue_summary}"
  human:
    when: { slot: issue, set: true }
    human: { notify: [push], inbox: support }   # phase 4
finish: { schema: { type: object, required: [answer], properties: {
            answer: { type: string }, facts: { type: array, items: { type: string } },
            needs_human: { type: boolean } } } }
on_tool_unavailable: reject             # reject | skip
publish:
  origins: ["https://www.example.com"]
  idle_ttl: 30m
  retention_days: 30
  output_filter: { patterns: { invoice: "RE-\\d{6}", customer: "K-\\d{5}" } }
```

A sub-agent's spec uses the same layout. It has no `state`, `routes` or
`publish`. Its input is the rendered `task`, and it must end in `finish`.

`tool_resources.<tool>` holds:
- **`bind`**: arguments the gateway fills in. They are removed from the schema
  the model sees, and the model's value for them is ignored. A source is either
  a slot path as a string (`"verified"`, `"verified.customer_id"`) or a fixed
  value written `{const: …}`. The explicit `const` keeps a literal that happens
  to match a slot name from being read as one. A route's `bind` uses the same
  form; a sub-agent has no `state`, so its own binds can only be `const`, and
  subject values reach it through the route's `bind`.
- **`permission`**: `always_allow`, the default, or `always_ask`, which suspends
  the call for human approval (phase 4).

**Checks when a spec is validated** (on save, again on publish):
- Every reference must exist and be granted to this agent's principal.
- Every `{slot}` in a template must exist in `state`.
- Every `bind` source must be a slot whose provenance cannot be `llm`.
- Every gate must type-check against the slots (§4).
- The sub-agent graph must be acyclic and at most 3 levels deep.

### What #84 built

- **Migration `0080_agents.sql`** creates the three tables above as written
  (`0078`/`0079` were left to concurrent branches). Rows live in
  `aiplane-core::server::db::agents`; every mutation writes an `agent_audit` row
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
    regexes, the `finish` schema (through `FinishContract::new`). Pools, tools,
    MCP tools (via their connector), skills and verifier connectors must be
    granted. Sub-agents must be existing agent ids, and not the agent itself.
    Gate leaves, templates and `set_by: verifier:<id>` must name declared slots
    and verifiers. Bind sources must be non-`llm` slots or `const`. Gates are
    checked for shape only.
  - *Publish*: everything above against the grants as they are now, plus
    `main.pool`, instructions, and every routed sub-agent having a live version.
  - Still open: gate type checks (#86), cycle and depth checks of the sub-agent
    graph (#88), slot run-time semantics (#85).
- **Shares.** The holder must have `can_manage_agents` when the share is
  written. For a user that means through their groups; a group needs the flag
  or `is_admin`. The caller must also hold the permission on every request.
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

**As built (#83, migration `0081`).** The rebuild is as above without
`visitor_id`: `visitor_sessions` does not exist yet, so #91 adds that column
together with its table. A second CHECK, `principal_id IS NULL OR shared = 0`,
keeps an agent conversation out of the "anyone with the link" read path.
`parent_turn_id` has no foreign key: the child run stays an auditable record
when the parent turn is gone. `session_core::db` gains `SessionOwner`,
`create_principal_session` (`NewRunSession`), `get_principal_session` and
`session_owner`; every person-facing query is unchanged and simply never
matches a row whose `user_id` is `NULL`. The chat sweeper for expired
suspensions skips principal-owned runs; the agent path resumes those (#88,
#91). Because seven tables cascade from `chat_sessions`, migrations now run
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
| `request_human(reason)` | main agent, when a `human` route exists | phase 4 |
| `verify_<id>(…)` | main agent | the verifier flow (#95) |
| `finish(result)` | sub-agents, and headless runs that opt in | ends the run. `result` is checked against the finish schema |

`forward_request` takes **no arguments** about the route or the subject.
- The router decides from state.
- With `kind: classifier`, a separate small call returns one route name from an
  enum. It can only pick among routes whose gate is already open.

### The call chain

```rust
pub struct RunChain {
    pub root_session: String,           // visitor conversation
    pub visitor_id: Option<String>,
    pub frames: Vec<Frame>,             // main agent, then each sub-agent hop
}
pub struct Frame { pub principal_id: String, pub version: i64, pub via_tool_call: Option<String> }
```

`RunChain` rides in `ToolContext.run`. These records carry the serialized chain:
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
fourth level (`ChainTooDeep`).

- `ToolContext.run: Option<Arc<RunChain>>`; `ToolContext::agent_active()` is
  `run.is_some()`. Its running frame must be `ToolContext.principal`:
  `headless::drive` refuses a run whose chain names a different principal
  before any round.
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
- `mcp_tool_audit` has a `chain` column (migration `0081`), set on every MCP
  call inside an agent run. Usage rows do not carry the chain yet; that comes
  with the per-agent limits and budgets (#92).
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
`{decision, value?, request_id?}`; the agent/public route comes with #91.
`child_turn` exists but nothing sets it: ancestor suspension and
innermost-first resume arrive with sub-agent dispatch (#88). Details in
[`tools-rbac.md`](tools-rbac.md#suspend-and-resume).

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
posts it to `POST /api/v0/embed/secure-input/{request_id}`. That resolves a
`secure_input` suspension (§3) directly. The code never enters model context or
the stored transcript.

**Output policy.** A public main agent uses `OutputPolicy::Buffered`. Each
answer is held until it passes the output filter (#89), then sent as one block,
while `status` events keep the widget alive. Token-by-token streaming and a
filter that sees the whole answer cannot both hold, and the filter wins for
untrusted audiences.

## 6. Crate placement

The rule from `AGENTS.md`: put code as high as it will go, and never reference
upward.

| Piece | Crate | Why there |
|---|---|---|
| Migrations; db modules for `system_principals`, `principal_grants`, `system_tokens`, `agents`, `agent_versions`, `agent_shares`, `agent_embed_keys`, `visitor_sessions`, `agent_state`, `agent_audit`; the `can_manage_agents` resolver check; `Principal`, `GrantSet` | `aiplane-core` | identity and rows sit below every consumer; no feature or `AppState` named |
| `chat_turn_suspensions` db fns; `suspended` status; new `chat_json` events (`suspended`, `state`, `gate`) | `session-core` | the chat substrate owns turn lifecycle and the SSE protocol |
| Spec types and validation, the gate evaluator, the schema-subset validator, template rendering | `aiplane-runtime` (`agents/`) | the lowest crate that needs them at run time; `aiplane-api` validates on save through it |
| `ToolContext.principal`/`run`, `RunProfile`, finish/budget/trim/repeat/suspend, `AgentToolSource` (synthetic tools, router, dispatch), output filter, tool-output injection hook, principal-aware tool/skill/MCP/pool resolution | `aiplane-runtime` | they are the loop and the tool machinery |
| Verifier tools (`mcp_code`, lookup), `request_human` | `aiplane-tools` | tool implementations; registered like every other tool |
| `/api/v0/agents/*`, `/api/v0/system-principals/*`, grants, shares, versions, embed keys, HiL inbox, the resume endpoint, the internal test chat | `aiplane-api` | JSON handlers |
| `/api/v0/embed/*` routes and CORS, `gws_`/`gwv_` bearer dispatch | `gateway` | routing glue only |
| Builder UI, test chat, inbox | `web/` (SPA) | daisyUI + Tailwind, all strings through Fluent |
| Embed widget | `web/embed/`, its own Vite entry built to `target/frontend/build/embed.js` | must not pull in the SPA; strings still come from the shared catalogs |

Nothing here needs a new Cargo dependency. Host-JWT verification uses the
existing `jsonwebtoken`, patterns use `regex`, and hashing uses the token
helpers.

## 7. Issue map

| Issue | Sections | Scope change from this design |
|---|---|---|
| #77 non-person principals, system tokens | §1 | Separate `system_principals`, `principal_grants` and `system_tokens` tables (`gws_`); `gateway_groups.can_manage_agents`; new connector scope `agent`; usage and audit get `principal_kind` |
| #78 finish contract | §3 RunProfile, §4 validator | `finish` is a synthetic tool; payload checked by the schema-subset validator |
| #79 budgets | §3 | `Budget` in `RunProfile`; chat derives it from `Effort` |
| #80 trim tool results | §3 | as described |
| #81 repeated calls | §3 | 3 identical calls warn, the 4th ends the run as `incomplete` |
| #82 suspend/resume | §3 | `chat_turn_suspensions` in session-core; `suspended` event; nested suspensions |
| #83 run identity, call chain | §1, §3 | `ToolContext.principal` replaces `user_id`/`roles`; `RunChain`; `chat_sessions` owner becomes user **or** principal (table rebuild); `agent_audit` |
| #84 agent definition | §2 | `agents` keyed by principal; `draft_spec` plus immutable `agent_versions`; shares: every share needs the permission |
| #85 typed state | §3 State | `agent_state` table; `set_by` list per slot; `subject` slot type |
| #86 gates | §4 | JSON condition tree instead of the string expressions in the epic's example; subject-bound routes must require a non-`llm` provenance |
| #87 router | §3 synthetic tools | `forward_request()` with no route argument; classifier picks only among open routes |
| #88 sub-agents | §2, §3 dispatch | a sub-agent is a separate agent (own principal, grants, versions), run as a child session |
| #89 output filter | §5 output policy | public main agents are buffered per answer |
| #90 builder UI, test chat | §2, §6 | draft runs only in the test chat |
| #91 public endpoint, visitor sessions | §5 | `gwe_` embed keys, `gwv_` visitor tokens in `sessionStorage`, fetch-streamed events, CORS only on `/api/v0/embed/*` |
| #92 limits, budget, pools, retention | §1, §5 | limits subject `system`; per-visitor and per-IP buckets; retention sweeps agent conversations |
| #93 injection scanning | §6 | a hook on tool results inside the runner, recorded in `agent_audit` |
| #94 widget | §5, §6 | script in shadow DOM, not an iframe; own Vite entry |
| #95 verifiers | §2 `verifiers`, §5 secure input | secure input resolves a `secure_input` suspension; host JWT through `jsonwebtoken` |
| #96 human in the loop | §3 suspend/resume | builds on `chat_turn_suspensions`; `human` route kind |
| #97 later | — | unchanged |

## Deferred

- **Streaming on public agents.** The first release buffers each answer of a
  public main agent until the output filter has checked it. A streaming mode for
  agents without an output filter can follow once the filter exists.
