// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! `/api/v0/agents` — agent definitions: drafts, immutable versions with a
//! live pointer, and shares (`docs/agents.md` §2).
//!
//! Every route needs the agent-management permission, and every route on one
//! agent also needs a share on it: `read` to see it, `write` to change it.
//! A share takes effect only for a holder of the permission, so a share is
//! refused for anyone who lacks it, and a holder who loses it loses access
//! with it. An agent nobody shares is invisible: it answers 404, not 403.
//! Admins are the exception: they hold `write` on every agent without a
//! share, so an agent whose last writer left can always be recovered.
//!
//! The agent's grants are its principal's, managed through
//! `/api/v0/system-principals/{id}/grants` with the grant-time cap from #77;
//! those routes check the share here too ([`guard_agent_principal`]).
//!
//! Embed keys (`/embed-keys`) follow the same share rules: `read` lists them,
//! `write` creates and revokes them. The public side is `pages::embed`.

use std::sync::Arc;

use rama::http::service::web::extract::State;
use rama::http::{Request, Response, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

use super::json_principals::require_agent_manager;
use super::{bad_request, internal, json_error, json_ok, no_content, not_found, raw_path_segment};
use aiplane_core::server::auth::token;
use aiplane_core::server::db::agents::{self as agents_db, Access, ShareChange, SubjectKind};
use aiplane_core::server::db::{
    agent_analytics, agent_audit, embed_keys, gateway_groups, system_principals as sp_db, users,
};
use aiplane_core::server::principal::GrantSet;
use aiplane_runtime::agents::spec::{self, SpecContext, SpecIssue, Stage};
use aiplane_runtime::rama_server::state::RamaState;

macro_rules! or_return {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(resp) => return resp,
        }
    };
}

fn group_ids(state: &RamaState, user: &users::User) -> Vec<String> {
    state.rbac.role_ids_for(&user.roles)
}

/// The caller's access to agent `id`: `write` for an admin, otherwise their
/// strongest share — a 404 when they hold none, a 403 when it is weaker than
/// `need`.
async fn access(
    state: &RamaState,
    user: &users::User,
    id: &str,
    need: Access,
) -> Result<Option<Access>, Response> {
    let groups = group_ids(state, user);
    if state.rbac.is_admin(&groups) {
        return Ok(Some(Access::Write));
    }
    let held = agents_db::access_for(&state.db, id, &user.id, &groups)
        .await
        .map_err(internal)?;
    match held {
        Some(a) if a >= need => Ok(Some(a)),
        Some(_) => Err(json_error(
            StatusCode::FORBIDDEN,
            "agent_write_required",
            "you can read this agent but not change it — ask someone with a `write` share to \
             upgrade yours",
        )),
        None => Err(not_found(format!(
            "there is no agent `{id}` shared with you — ask its owner for a share"
        ))),
    }
}

/// The agent named by the path segment `from_end` back, if the caller holds
/// `need` on it.
pub(super) async fn agent_at(
    state: &RamaState,
    req: &Request,
    user: &users::User,
    from_end: usize,
    need: Access,
) -> Result<(agents_db::AgentRow, Access), Response> {
    let Some(id) = raw_path_segment(req, from_end) else {
        return Err(bad_request("the URL is missing the agent id"));
    };
    let Some(agent) = agents_db::get(&state.db, &id).await.map_err(internal)? else {
        return Err(not_found(format!(
            "there is no agent `{id}` shared with you — ask its owner for a share"
        )));
    };
    let held = access(state, user, &id, need).await?;
    Ok((agent, held.unwrap_or(need)))
}

/// For the `/api/v0/system-principals` routes: when the principal is an
/// agent, the caller needs a share on it as well as the permission. A
/// principal that is not an agent passes.
pub(crate) async fn guard_agent_principal(
    state: &RamaState,
    user: &users::User,
    principal_id: &str,
    need: Access,
) -> Result<(), Response> {
    if agents_db::get(&state.db, principal_id)
        .await
        .map_err(internal)?
        .is_none()
    {
        return Ok(());
    }
    access(state, user, principal_id, need).await.map(|_| ())
}

pub(super) fn parse_spec(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

fn agent_json(a: &agents_db::AgentRow, access: Access) -> Value {
    let p = &a.principal;
    json!({
        "id": p.id,
        "name": p.name,
        "display": p.display,
        "description": p.description,
        "created_by": p.created_by,
        "created_at": a.created_at,
        "updated_at": a.updated_at,
        "disabled_at": p.disabled_at,
        "live_version": a.live_version,
        "access": access.as_str(),
    })
}

fn share_json(s: &agents_db::ShareRow) -> Value {
    json!({
        "subject_kind": s.subject_kind.as_str(),
        "subject_id": s.subject_id,
        "access": s.access.as_str(),
    })
}

fn version_json(v: &agents_db::VersionRow) -> Value {
    json!({
        "version": v.version,
        "spec": parse_spec(&v.spec),
        "published_by": v.published_by,
        "published_at": v.published_at,
    })
}

/// Validate `spec` for agent `agent_id` against its grants as stored now.
async fn spec_issues(
    state: &RamaState,
    agent_id: &str,
    spec: &Value,
    stage: Stage,
) -> Result<Vec<SpecIssue>, Response> {
    let grants = sp_db::grants(&state.db, agent_id).await.map_err(internal)?;
    let grants = GrantSet::new(grants.into_iter().map(|g| (g.kind, g.reference)));
    let agents = agents_db::publication_status(&state.db)
        .await
        .map_err(internal)?;
    let live_specs = agents_db::live_specs(&state.db)
        .await
        .map_err(internal)?
        .into_iter()
        .map(|(id, text)| (id, parse_spec(&text)))
        .collect();
    Ok(spec::validate(
        spec,
        &SpecContext {
            agent_id,
            grants: &grants,
            agents: &agents,
            live_specs: &live_specs,
        },
        stage,
    ))
}

/// Seal every verifier secret and A2A route credential once the spec is
/// valid (the plaintext is checked), before it is stored or echoed: it never
/// rests in a draft, a version or the audit trail in clear.
fn seal_secrets(state: &RamaState, spec: &mut Value) -> Result<(), Response> {
    aiplane_runtime::agents::verifier::host_jwt::seal_secrets(spec, &state.crypto)
        .and_then(|()| aiplane_runtime::agents::a2a_client::seal_secrets(spec, &state.crypto))
        .map_err(internal)
}

fn invalid_spec(what: &str, issues: &[SpecIssue]) -> Response {
    let first = &issues[0];
    let at = if first.path.is_empty() {
        "the spec".to_string()
    } else {
        format!("`{}`", first.path)
    };
    let more = match issues.len() {
        1 => String::new(),
        n => format!(" (and {} more — see `issues`)", n - 1),
    };
    json_ok(
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({
            "error": {
                "message": format!("cannot {what}: at {at}, {}{more}", first.message),
                "type": "invalid_agent_spec",
                "code": "invalid_agent_spec",
                "issues": issues,
            }
        }),
    )
}

async fn require_valid(
    state: &RamaState,
    agent_id: &str,
    spec: &Value,
    stage: Stage,
    what: &str,
) -> Result<(), Response> {
    let issues = spec_issues(state, agent_id, spec, stage).await?;
    if issues.is_empty() {
        Ok(())
    } else {
        Err(invalid_spec(what, &issues))
    }
}

/// GET /api/v0/agents — the agents shared with the caller; every agent for
/// an admin.
pub async fn list(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let user = or_return!(require_agent_manager(&state, &req).await);
    let groups = group_ids(&state, &user);
    let rows = if state.rbac.is_admin(&groups) {
        agents_db::list_all(&state.db)
            .await
            .map(|rows| rows.into_iter().map(|a| (a, Access::Write)).collect())
    } else {
        agents_db::list_shared_with(&state.db, &user.id, &groups).await
    };
    let rows: Vec<(agents_db::AgentRow, Access)> = match rows {
        Ok(rows) => rows,
        Err(err) => return internal(err),
    };
    let agents: Vec<Value> = rows.iter().map(|(a, acc)| agent_json(a, *acc)).collect();
    json_ok(StatusCode::OK, json!({ "agents": agents }))
}

#[derive(Deserialize)]
pub struct CreateBody {
    pub name: String,
    #[serde(default)]
    pub display: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub spec: Option<Value>,
}

/// POST /api/v0/agents — an agent with a fresh principal (no grants) and a
/// `write` share for its creator.
pub async fn create(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let user = or_return!(require_agent_manager(&state, &req).await);
    let body: CreateBody = or_return!(super::read_json(req.into_body(), "the agent body").await);
    let name = body.name.trim();
    if let Some(reason) = sp_db::invalid_name_reason(name) {
        return bad_request(reason);
    }
    let mut spec = body.spec.unwrap_or_else(|| json!({}));
    // The principal does not exist yet, so it holds nothing: a spec naming
    // any resource fails here, and its hint says how to grant one.
    or_return!(require_valid(&state, "{id}", &spec, Stage::Draft, "create the agent").await);
    or_return!(seal_secrets(&state, &mut spec));
    let display = body
        .display
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .unwrap_or(name);
    let created = agents_db::create(
        &state.db,
        &sp_db::NewPrincipal {
            name,
            display,
            description: body.description.trim(),
        },
        &spec.to_string(),
        &user.id,
    )
    .await;
    match created {
        Ok(Some(a)) => {
            let mut v = agent_json(&a, Access::Write);
            v["draft_spec"] = spec;
            json_ok(StatusCode::CREATED, json!({ "agent": v }))
        }
        Ok(None) => json_error(
            StatusCode::CONFLICT,
            "conflict",
            &format!(
                "a system principal named `{name}` already exists, and an agent's name is its \
                 principal's — pick another name"
            ),
        ),
        Err(err) => internal(err),
    }
}

/// GET /api/v0/agents/{id} — the agent, its draft and live spec, what
/// blocks publishing the draft, its limits and what has been spent against
/// them, its grants, shares and audit trail.
pub async fn detail(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let user = or_return!(require_agent_manager(&state, &req).await);
    let (agent, held) = or_return!(agent_at(&state, &req, &user, 0, Access::Read).await);
    let id = &agent.principal.id;
    let (grants, shares, audit) = match tokio::try_join!(
        sp_db::grants(&state.db, id),
        agents_db::shares(&state.db, id),
        agent_audit::for_principal(&state.db, id),
    ) {
        Ok(v) => v,
        Err(err) => return internal(err),
    };
    let live_spec = match agent.live_version {
        Some(n) => match agents_db::version(&state.db, id, n).await {
            Ok(v) => v.map(|v| parse_spec(&v.spec)),
            Err(err) => return internal(err),
        },
        None => None,
    };
    let draft = parse_spec(&agent.draft_spec);
    let publish_issues = or_return!(spec_issues(&state, id, &draft, Stage::Publish).await);

    let limits = aiplane_runtime::agents::embed::limits_view(
        &state,
        id,
        live_spec.as_ref(),
        jiff::Timestamp::now(),
    )
    .await;

    let mut v = agent_json(&agent, held);
    v["draft_spec"] = draft;
    v["live_spec"] = live_spec.unwrap_or(Value::Null);
    v["limits"] = limits;
    v["publish_issues"] = json!(publish_issues);
    v["grants"] = grants
        .iter()
        .map(|g| {
            json!({
                "kind": g.kind.as_str(),
                "ref": g.reference,
                "granted_by": g.granted_by,
                "granted_at": g.granted_at,
            })
        })
        .collect();
    v["shares"] = shares.iter().map(share_json).collect();
    v["audit"] = audit
        .iter()
        .map(|e| {
            json!({
                "kind": e.kind,
                "actor_id": e.actor_id,
                "detail": e.detail,
                "created_at": e.created_at,
            })
        })
        .collect();
    json_ok(StatusCode::OK, json!({ "agent": v }))
}

#[derive(Deserialize)]
pub struct DraftBody {
    pub spec: Value,
}

/// PUT /api/v0/agents/{id}/draft — replace the draft. What the live version
/// serves does not change.
pub async fn update_draft(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let user = or_return!(require_agent_manager(&state, &req).await);
    let (agent, _) = or_return!(agent_at(&state, &req, &user, 1, Access::Write).await);
    let id = agent.principal.id;
    let mut body: DraftBody = or_return!(super::read_json(req.into_body(), "the draft body").await);
    or_return!(require_valid(&state, &id, &body.spec, Stage::Draft, "save the draft").await);
    or_return!(seal_secrets(&state, &mut body.spec));
    match agents_db::update_draft(&state.db, &id, &body.spec.to_string(), &user.id).await {
        Ok(true) => json_ok(
            StatusCode::OK,
            json!({ "draft_spec": body.spec, "live_version": agent.live_version }),
        ),
        Ok(false) => not_found("the agent was deleted while its draft was being saved"),
        Err(err) => internal(err),
    }
}

/// POST /api/v0/agents/{id}/publish — validate the draft for running and
/// snapshot it as the next version, which becomes live.
pub async fn publish(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let user = or_return!(require_agent_manager(&state, &req).await);
    let (agent, _) = or_return!(agent_at(&state, &req, &user, 1, Access::Write).await);
    let id = &agent.principal.id;
    let draft = parse_spec(&agent.draft_spec);
    or_return!(require_valid(&state, id, &draft, Stage::Publish, "publish the agent").await);
    or_return!(super::json_agent_tests::require_green_suite(&state, &agent).await);
    match agents_db::publish(&state.db, id, &agent.draft_spec, &user.id).await {
        Ok(Some(version)) => json_ok(
            StatusCode::CREATED,
            json!({ "version": version, "live_version": version }),
        ),
        Ok(None) => not_found("the agent was deleted while it was being published"),
        Err(err) => internal(err),
    }
}

/// GET /api/v0/agents/{id}/versions — every published version, newest
/// first.
pub async fn versions(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let user = or_return!(require_agent_manager(&state, &req).await);
    let (agent, _) = or_return!(agent_at(&state, &req, &user, 1, Access::Read).await);
    match agents_db::versions(&state.db, &agent.principal.id).await {
        Ok(rows) => json_ok(
            StatusCode::OK,
            json!({
                "live_version": agent.live_version,
                "versions": rows.iter().map(version_json).collect::<Vec<_>>(),
            }),
        ),
        Err(err) => internal(err),
    }
}

const MAX_ANALYTICS_DAYS: i64 = 366;

/// An analytics bound: an RFC 3339 instant, or a `YYYY-MM-DD` day in UTC. A
/// day as `to` means the end of that day, so `from=2026-10-01&to=2026-10-07`
/// covers seven whole days.
fn analytics_bound(name: &str, value: &str, end_of_day: bool) -> Result<jiff::Timestamp, Response> {
    use jiff::ToSpan;
    if let Ok(at) = value.parse::<jiff::Timestamp>() {
        return Ok(at);
    }
    let day = value.parse::<jiff::civil::Date>().map_err(|_| {
        bad_request(format!(
            "`{name}` is `{value}`, which is neither an RFC 3339 time nor a YYYY-MM-DD day — \
             write it like `2026-10-01` or `2026-10-01T00:00:00Z`"
        ))
    })?;
    let start = day
        .to_zoned(jiff::tz::TimeZone::UTC)
        .map_err(|err| bad_request(format!("`{name}` is out of range: {err}")))?
        .timestamp();
    Ok(if end_of_day {
        start + 24.hours()
    } else {
        start
    })
}

/// GET /api/v0/agents/{id}/analytics?from=&to=&version= — counts of what the
/// agent did in the range (default: the last 30 days), without any visitor
/// content. `version` narrows to one published version; builder test
/// conversations are never counted.
pub async fn analytics(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    use jiff::ToSpan;
    let user = or_return!(require_agent_manager(&state, &req).await);
    let (agent, _) = or_return!(agent_at(&state, &req, &user, 1, Access::Read).await);
    let query: std::collections::HashMap<String, String> =
        serde_urlencoded::from_str(req.uri().query().unwrap_or("")).unwrap_or_default();
    let to = match query.get("to") {
        Some(v) => or_return!(analytics_bound("to", v, true)),
        None => jiff::Timestamp::now(),
    };
    let from = match query.get("from") {
        Some(v) => or_return!(analytics_bound("from", v, false)),
        None => to - (30 * 24).hours(),
    };
    if from >= to {
        return bad_request("`from` must be before `to` — swap them or widen the range");
    }
    if to.duration_since(from).as_secs() > MAX_ANALYTICS_DAYS * 86_400 {
        return bad_request(format!(
            "the range is longer than {MAX_ANALYTICS_DAYS} days — ask for a shorter one"
        ));
    }
    let version = match query.get("version") {
        None => None,
        Some(v) => match v.parse::<i64>() {
            Ok(n) if n >= 1 => Some(n),
            _ => {
                return bad_request(format!(
                    "`version` is `{v}`; it must be a published version number, 1 or more — list \
                     them with GET /api/v0/agents/{}/versions",
                    agent.principal.id
                ));
            }
        },
    };
    match agent_analytics::compute(
        &state.db,
        &agent.principal.id,
        agent_analytics::Range { from, to },
        version,
    )
    .await
    {
        Ok(a) => {
            let mut body = serde_json::to_value(a).expect("analytics serialize");
            body["currency"] = json!(state.config().usage.currency);
            json_ok(StatusCode::OK, body)
        }
        Err(err) => internal(err),
    }
}

#[derive(Deserialize)]
pub struct LiveBody {
    pub version: i64,
}

/// POST /api/v0/agents/{id}/live — make an existing version live: a
/// rollback, or forward again. Not re-validated: grants are not versioned,
/// and a reference whose grant was revoked meets default-deny at run time.
pub async fn set_live(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let user = or_return!(require_agent_manager(&state, &req).await);
    let (agent, _) = or_return!(agent_at(&state, &req, &user, 1, Access::Write).await);
    let id = agent.principal.id;
    let body: LiveBody =
        or_return!(super::read_json(req.into_body(), "the live-version body").await);
    match agents_db::set_live(&state.db, &id, body.version, &user.id).await {
        Ok(true) => json_ok(StatusCode::OK, json!({ "live_version": body.version })),
        Ok(false) => not_found(format!(
            "`{}` has no version {} — list them with GET /api/v0/agents/{id}/versions",
            agent.principal.name, body.version
        )),
        Err(err) => internal(err),
    }
}

/// GET /api/v0/agents/{id}/shares
pub async fn shares(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let user = or_return!(require_agent_manager(&state, &req).await);
    let (agent, _) = or_return!(agent_at(&state, &req, &user, 1, Access::Read).await);
    match agents_db::shares(&state.db, &agent.principal.id).await {
        Ok(rows) => json_ok(
            StatusCode::OK,
            json!({ "shares": rows.iter().map(share_json).collect::<Vec<_>>() }),
        ),
        Err(err) => internal(err),
    }
}

#[derive(Deserialize)]
pub struct ShareBody {
    pub subject_kind: String,
    pub subject_id: String,
    #[serde(default)]
    pub access: Option<String>,
}

fn parse_subject(body: &ShareBody) -> Result<(SubjectKind, &str), Response> {
    let kind = SubjectKind::parse(&body.subject_kind).ok_or_else(|| {
        bad_request(format!(
            "`{}` is not a share subject — use `user` or `group`",
            body.subject_kind
        ))
    })?;
    let subject = body.subject_id.trim();
    if subject.is_empty() {
        return Err(bad_request("a share needs a `subject_id`"));
    }
    Ok((kind, subject))
}

/// A share only takes effect for a holder of the agent-management
/// permission, so one for anybody else is refused rather than stored inert.
async fn require_manager_subject(
    state: &RamaState,
    kind: SubjectKind,
    subject: &str,
) -> Result<(), Response> {
    let holds = match kind {
        SubjectKind::User => {
            let Some(u) = users::find_by_id(&state.db, subject)
                .await
                .map_err(internal)?
            else {
                return Err(not_found(format!("there is no user `{subject}`")));
            };
            state.rbac.can_manage_agents(&group_ids(state, &u))
        }
        SubjectKind::Group => {
            let groups = gateway_groups::list_groups(&state.db)
                .await
                .map_err(internal)?;
            let Some(g) = groups.into_iter().find(|g| g.name == subject) else {
                return Err(not_found(format!("there is no group `{subject}`")));
            };
            g.is_admin || g.can_manage_agents
        }
    };
    if holds {
        return Ok(());
    }
    Err(json_error(
        StatusCode::UNPROCESSABLE_ENTITY,
        "share_needs_agent_manager",
        &format!(
            "cannot share with {} `{subject}`: a share only works for holders of the \
             agent-management permission, because it shows the spec and the agent's \
             conversations. Ask an admin to enable `can_manage_agents` on {}, then share again.",
            kind.as_str(),
            match kind {
                SubjectKind::User => "one of their groups",
                SubjectKind::Group => "that group",
            }
        ),
    ))
}

fn last_writer() -> Response {
    json_error(
        StatusCode::CONFLICT,
        "last_writer",
        "this is the agent's last `write` share — give someone else `write` first, or the agent \
         could never be edited or shared again",
    )
}

/// POST /api/v0/agents/{id}/shares — add a share or change its access.
pub async fn share(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let user = or_return!(require_agent_manager(&state, &req).await);
    let (agent, _) = or_return!(agent_at(&state, &req, &user, 1, Access::Write).await);
    let body: ShareBody = or_return!(super::read_json(req.into_body(), "the share body").await);
    let (kind, subject) = or_return!(parse_subject(&body));
    let access = match body.access.as_deref().map(Access::parse) {
        Some(Some(a)) => a,
        _ => return bad_request("a share needs `access`: `read` or `write`"),
    };
    or_return!(require_manager_subject(&state, kind, subject).await);
    match agents_db::set_share(
        &state.db,
        &agent.principal.id,
        kind,
        subject,
        access,
        &user.id,
    )
    .await
    {
        Ok(ShareChange::LastWriter) => last_writer(),
        Ok(change) => json_ok(
            if change == ShareChange::Changed {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            },
            json!({ "subject_kind": kind.as_str(), "subject_id": subject, "access": access.as_str() }),
        ),
        Err(err) => internal(err),
    }
}

/// POST /api/v0/agents/{id}/shares/revoke — remove one share.
pub async fn revoke_share(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let user = or_return!(require_agent_manager(&state, &req).await);
    let (agent, _) = or_return!(agent_at(&state, &req, &user, 2, Access::Write).await);
    let body: ShareBody = or_return!(super::read_json(req.into_body(), "the share body").await);
    let (kind, subject) = or_return!(parse_subject(&body));
    match agents_db::remove_share(&state.db, &agent.principal.id, kind, subject, &user.id).await {
        Ok(ShareChange::LastWriter) => last_writer(),
        Ok(ShareChange::NotFound) => not_found(format!(
            "`{}` is not shared with {} `{subject}`",
            agent.principal.name,
            kind.as_str()
        )),
        Ok(_) => no_content(),
        Err(err) => internal(err),
    }
}

/// DELETE /api/v0/agents/{id} — the agent and its principal, with every
/// grant, token, version and share. The audit trail stays.
pub async fn delete(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let user = or_return!(require_agent_manager(&state, &req).await);
    let (agent, _) = or_return!(agent_at(&state, &req, &user, 0, Access::Write).await);
    match agents_db::delete(&state.db, &agent.principal.id, &user.id).await {
        Ok(_) => no_content(),
        Err(err) => internal(err),
    }
}

fn embed_key_json(k: &embed_keys::EmbedKey) -> Value {
    json!({
        "id": k.id,
        "name": k.name,
        "origins": k.origins,
        "created_by": k.created_by,
        "created_at": k.created_at,
        "revoked_at": k.revoked_at,
    })
}

/// GET /api/v0/agents/{id}/embed-keys — every key, revoked ones included,
/// never the key itself.
pub async fn embed_keys_list(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let user = or_return!(require_agent_manager(&state, &req).await);
    let (agent, _) = or_return!(agent_at(&state, &req, &user, 1, Access::Read).await);
    match embed_keys::list(&state.db, &agent.principal.id).await {
        Ok(keys) => json_ok(
            StatusCode::OK,
            json!({ "embed_keys": keys.iter().map(embed_key_json).collect::<Vec<_>>() }),
        ),
        Err(err) => internal(err),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbedKeyBody {
    pub name: String,
    pub origins: Vec<String>,
}

/// The origins as stored: trimmed, de-duplicated, each exactly
/// `scheme://host[:port]`, at least one.
fn parse_origins(origins: &[String]) -> Result<Vec<String>, Response> {
    let mut out: Vec<String> = Vec::new();
    for origin in origins.iter().map(|o| o.trim()) {
        if !spec::is_origin(origin) {
            return Err(bad_request(format!(
                "`{origin}` is not an origin — write exactly what the browser sends in `Origin`: \
                 `https://host` or `https://host:port`, without a path or trailing slash"
            )));
        }
        if !out.iter().any(|o| o == origin) {
            out.push(origin.to_string());
        }
    }
    if out.is_empty() {
        return Err(bad_request(
            "an embed key needs at least one origin, e.g. `https://www.example.com` — no website \
             could use it otherwise",
        ));
    }
    Ok(out)
}

/// POST /api/v0/agents/{id}/embed-keys — a new `gwe_` key for the listed
/// origins. The key is in this response only.
pub async fn embed_key_create(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let user = or_return!(require_agent_manager(&state, &req).await);
    let (agent, _) = or_return!(agent_at(&state, &req, &user, 1, Access::Write).await);
    let body: EmbedKeyBody =
        or_return!(super::read_json(req.into_body(), "the embed key body").await);
    let name = body.name.trim();
    if name.is_empty() {
        return bad_request("an embed key needs a `name`, e.g. the website it is for");
    }
    let origins = or_return!(parse_origins(&body.origins));
    let (key, key_hash) = token::mint_embed_key();
    match embed_keys::create(
        &state.db,
        &embed_keys::NewEmbedKey {
            principal_id: &agent.principal.id,
            name,
            origins: &origins,
            key_hash: &key_hash,
        },
        &user.id,
    )
    .await
    {
        Ok(k) => json_ok(
            StatusCode::CREATED,
            json!({ "embed_key": embed_key_json(&k), "key": key }),
        ),
        Err(err) => internal(err),
    }
}

/// POST /api/v0/agents/{id}/embed-keys/{key_id}/revoke — websites using the
/// key stop working at once, open visitor conversations included.
pub async fn embed_key_revoke(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let user = or_return!(require_agent_manager(&state, &req).await);
    let (agent, _) = or_return!(agent_at(&state, &req, &user, 3, Access::Write).await);
    let Some(key_id) = raw_path_segment(&req, 1) else {
        return bad_request("the URL is missing the embed key id");
    };
    match embed_keys::revoke(&state.db, &agent.principal.id, &key_id, &user.id).await {
        Ok(true) => no_content(),
        Ok(false) => not_found(format!(
            "`{}` has no live embed key `{key_id}` — list them with GET /api/v0/agents/{}/embed-keys",
            agent.principal.name, agent.principal.id
        )),
        Err(err) => internal(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_are_exact_deduplicated_and_never_empty() {
        let o = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            parse_origins(&o(&[
                " https://a.example ",
                "https://a.example",
                "http://localhost:5173"
            ]))
            .unwrap(),
            o(&["https://a.example", "http://localhost:5173"])
        );
        for bad in ["https://a.example/", "a.example", "*", "ftp://a.example"] {
            assert!(parse_origins(&o(&[bad])).is_err(), "{bad} accepted");
        }
        assert!(parse_origins(&[]).is_err());
    }

    fn body(kind: &str, id: &str) -> ShareBody {
        ShareBody {
            subject_kind: kind.into(),
            subject_id: id.into(),
            access: None,
        }
    }

    #[test]
    fn a_share_subject_is_a_user_or_a_group_with_an_id() {
        assert_eq!(
            parse_subject(&body("user", " bob ")).unwrap(),
            (SubjectKind::User, "bob")
        );
        assert_eq!(
            parse_subject(&body("group", "support")).unwrap(),
            (SubjectKind::Group, "support")
        );
        assert!(parse_subject(&body("role", "x")).is_err());
        assert!(parse_subject(&body("user", "  ")).is_err());
    }
}
