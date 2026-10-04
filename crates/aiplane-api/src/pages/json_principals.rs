// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! `/api/v0/system-principals` — create and configure non-person principals
//! (`docs/agents.md` §1).
//!
//! Every route needs the agent-management permission (`can_manage_agents` on
//! one of the caller's groups; admin implies it). A grant is capped at what
//! the manager holds *at grant time*, checked with the same rule that decides
//! the manager's own access to that resource, and is never re-derived from the
//! manager afterwards. Every change is written to `agent_audit` by the db
//! layer, in the same transaction as the change.
//!
//! An agent's principal (`docs/agents.md` §2) is also reachable here, so for
//! one of those the caller additionally needs a share on the agent — `read`
//! to see it, `write` to change it — exactly as on `/api/v0/agents`. Any
//! other principal is reachable only by its creator and by admins.
//!
//! A token carries every grant of its principal, so issuing one is held to
//! the grant-time cap for all of them at once; a non-admin's token is capped
//! again at every request, at what its minter holds then
//! (`grant_holding::capped_to_minter`).

use std::sync::Arc;

use jiff::{SignedDuration, Timestamp};
use rama::http::service::web::extract::State;
use rama::http::{Request, Response, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

use super::json_agents::guard_principal;
use super::{bad_request, internal, json_error, json_ok, no_content, not_found, raw_path_segment};
use aiplane_agents::db::agents::{self as agents_db, Access};
use aiplane_agents::db::{agent_audit, system_principals as sp_db};
use aiplane_core::server::auth::token;
use aiplane_core::server::db::users;
use aiplane_core::server::principal::GrantKind;
use aiplane_runtime::rama_server::state::RamaState;
use aiplane_runtime::server::grant_holding::{self, HoldRefusal};

/// Session + agent-management permission, or the 401/403 to return.
pub(crate) async fn require_agent_manager(
    state: &RamaState,
    req: &Request,
) -> Result<users::User, Response> {
    let (_, user) = super::require_session_json(state, req).await?;
    let role_ids = state.rbac.role_ids_for(&user.roles);
    if !state.rbac.can_manage_agents(&role_ids) {
        return Err(json_error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "managing system principals needs the agent-management permission — ask an admin \
             to enable `can_manage_agents` on one of your groups",
        ));
    }
    Ok(user)
}

macro_rules! require_agent_manager {
    ($state:expr, $req:expr) => {
        match require_agent_manager(&$state, &$req).await {
            Ok(user) => user,
            Err(resp) => return resp,
        }
    };
}

fn principal_json(p: &sp_db::PrincipalRow) -> Value {
    json!({
        "id": p.id,
        "name": p.name,
        "display": p.display,
        "description": p.description,
        "created_by": p.created_by,
        "created_at": p.created_at,
        "disabled_at": p.disabled_at,
    })
}

fn grant_json(g: &sp_db::GrantRow) -> Value {
    json!({
        "kind": g.kind.as_str(),
        "ref": g.reference,
        "granted_by": g.granted_by,
        "granted_at": g.granted_at,
    })
}

fn token_json(t: &sp_db::SystemToken) -> Value {
    json!({
        "id": t.id,
        "name": t.name,
        "created_by": t.created_by,
        "created_at": t.created_at,
        "last_used_at": t.last_used_at,
        "expires_at": t.expires_at,
        "revoked": t.revoked_at.is_some(),
    })
}

/// The principal named by the path segment `from_end` back, or the 404 —
/// also when `manager` may not reach it ([`guard_principal`]).
async fn principal_at(
    state: &RamaState,
    req: &Request,
    manager: &users::User,
    from_end: usize,
    need: Access,
) -> Result<sp_db::PrincipalRow, Response> {
    let Some(id) = raw_path_segment(req, from_end) else {
        return Err(bad_request("the URL is missing the principal id"));
    };
    let p = match sp_db::get(&state.db, &id).await {
        Ok(Some(p)) => p,
        Ok(None) => return Err(not_found(format!("there is no system principal `{id}`"))),
        Err(err) => return Err(internal(err)),
    };
    guard_principal(state, manager, &p, need).await?;
    Ok(p)
}

/// GET /api/v0/system-principals
pub async fn list(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let manager = require_agent_manager!(state, req);
    let rows = match sp_db::list(&state.db).await {
        Ok(rows) => rows,
        Err(err) => return internal(err),
    };
    let mut out = Vec::with_capacity(rows.len());
    for p in &rows {
        if guard_principal(&state, &manager, p, Access::Read)
            .await
            .is_err()
        {
            continue;
        }
        let grants = match sp_db::grants(&state.db, &p.id).await {
            Ok(g) => g,
            Err(err) => return internal(err),
        };
        let mut v = principal_json(p);
        v["grants"] = grants.iter().map(grant_json).collect();
        out.push(v);
    }
    json_ok(StatusCode::OK, json!({ "principals": out }))
}

#[derive(Deserialize)]
pub struct CreateBody {
    pub name: String,
    #[serde(default)]
    pub display: Option<String>,
    #[serde(default)]
    pub description: String,
}

/// POST /api/v0/system-principals — a new principal with no rights at all.
pub async fn create(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let manager = require_agent_manager!(state, req);
    let body: CreateBody = match super::read_json(req.into_body(), "the principal body").await {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let name = body.name.trim();
    if let Some(reason) = sp_db::invalid_name_reason(name) {
        return bad_request(reason);
    }
    let display = body
        .display
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .unwrap_or(name);
    let created = sp_db::create(
        &state.db,
        &sp_db::NewPrincipal {
            name,
            display,
            description: body.description.trim(),
        },
        &manager.id,
    )
    .await;
    match created {
        Ok(Some(p)) => {
            let mut v = principal_json(&p);
            v["grants"] = json!([]);
            json_ok(StatusCode::CREATED, json!({ "principal": v }))
        }
        Ok(None) => json_error(
            StatusCode::CONFLICT,
            "conflict",
            &format!("a system principal named `{name}` already exists — pick another name"),
        ),
        Err(err) => internal(err),
    }
}

/// GET /api/v0/system-principals/{id} — the principal, its grants, tokens
/// and audit trail.
pub async fn detail(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let manager = require_agent_manager!(state, req);
    let p = match principal_at(&state, &req, &manager, 0, Access::Read).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let (grants, tokens, audit) = match tokio::try_join!(
        sp_db::grants(&state.db, &p.id),
        sp_db::tokens(&state.db, &p.id),
        agent_audit::for_principal(&state.db, &p.id),
    ) {
        Ok(v) => v,
        Err(err) => return internal(err),
    };
    let mut v = principal_json(&p);
    v["grants"] = grants.iter().map(grant_json).collect();
    v["tokens"] = tokens.iter().map(token_json).collect();
    v["audit"] = audit
        .iter()
        .map(|e| {
            json!({
                "kind": e.kind,
                "actor_id": e.actor_id,
                "chain": e.chain,
                "detail": e.detail,
                "created_at": e.created_at,
            })
        })
        .collect();
    json_ok(StatusCode::OK, json!({ "principal": v }))
}

/// POST /api/v0/system-principals/{id}/disable — disables it and revokes
/// every token it holds. Its grants stay, so re-enabling later is a
/// reconfiguration, not a rebuild.
pub async fn disable(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let manager = require_agent_manager!(state, req);
    let p = match principal_at(&state, &req, &manager, 1, Access::Write).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    if let Err(err) = sp_db::disable(&state.db, &p.id, &manager.id).await {
        return internal(err);
    }
    match sp_db::get(&state.db, &p.id).await {
        Ok(Some(p)) => json_ok(StatusCode::OK, json!({ "principal": principal_json(&p) })),
        Ok(None) => not_found("the principal was removed while it was being disabled"),
        Err(err) => internal(err),
    }
}

#[derive(Deserialize)]
pub struct GrantBody {
    pub kind: String,
    #[serde(rename = "ref")]
    pub reference: String,
}

fn parse_grant(body: &GrantBody) -> Result<(GrantKind, &str), Response> {
    let kind = GrantKind::parse(&body.kind).ok_or_else(|| {
        bad_request(format!(
            "`{}` is not a grant kind — use one of tool, connector, skill, rag_collection, pool, \
             a2a_caller, a2a_agent",
            body.kind
        ))
    })?;
    let reference = body.reference.trim();
    if reference.is_empty() {
        return Err(bad_request("a grant needs a `ref`"));
    }
    if reference.contains('*') {
        return Err(bad_request(
            "a system principal cannot be granted a wildcard — grant each resource by name",
        ));
    }
    Ok((kind, reference))
}

/// Whether `manager` holds `kind` `reference` today
/// ([`grant_holding::holds`]), or the response that says why it cannot be
/// granted at all.
async fn manager_holds(
    state: &RamaState,
    manager: &users::User,
    kind: GrantKind,
    reference: &str,
) -> Result<bool, Response> {
    if kind == GrantKind::A2aCaller
        && let Some(agent) = agents_db::get(&state.db, reference)
            .await
            .map_err(internal)?
    {
        guard_principal(state, manager, &agent.principal, Access::Write).await?;
    }
    grant_holding::holds(state, manager, kind, reference)
        .await
        .map_err(|refusal| match refusal {
            HoldRefusal::Missing(_) => not_found(refusal.to_string()),
            HoldRefusal::Invalid(why) => bad_request(why),
            HoldRefusal::AdminOnly(why) => {
                json_error(StatusCode::FORBIDDEN, "grant_exceeds_manager", &why)
            }
            HoldRefusal::Db(err) => internal(err),
        })
}

/// `Ok` when `manager` may hand out a token for `principal`: an admin always,
/// anyone else only while they hold every grant it has. A grant that could
/// not be made today (its resource is gone) counts as not held.
async fn manager_holds_every_grant(
    state: &RamaState,
    manager: &users::User,
    principal: &sp_db::PrincipalRow,
) -> Result<(), Response> {
    if state
        .rbac
        .is_admin(&state.rbac.role_ids_for(&manager.roles))
    {
        return Ok(());
    }
    let grants = sp_db::grants(&state.db, &principal.id)
        .await
        .map_err(internal)?;
    for g in &grants {
        let held = match manager_holds(state, manager, g.kind, &g.reference).await {
            Ok(held) => held,
            Err(resp) if resp.status() == StatusCode::INTERNAL_SERVER_ERROR => return Err(resp),
            Err(_) => false,
        };
        if !held {
            return Err(json_error(
                StatusCode::FORBIDDEN,
                "token_exceeds_manager",
                &format!(
                    "cannot issue a token for `{}`: it holds {} `{}`, which you do not hold \
                     yourself, and a token would hand it to whoever has the token. Ask an admin \
                     to issue the token, or revoke that grant first.",
                    principal.name,
                    kind_label(g.kind),
                    g.reference
                ),
            ));
        }
    }
    Ok(())
}

fn kind_label(kind: GrantKind) -> &'static str {
    match kind {
        GrantKind::Tool => "tool",
        GrantKind::Connector => "connector",
        GrantKind::Skill => "skill",
        GrantKind::RagCollection => "RAG collection",
        GrantKind::Model => "model",
        GrantKind::A2aCaller => "A2A caller",
        GrantKind::A2aAgent => "A2A agent",
    }
}

/// POST /api/v0/system-principals/{id}/grants — add one grant, capped at
/// what the caller holds right now.
pub async fn grant(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let manager = require_agent_manager!(state, req);
    let p = match principal_at(&state, &req, &manager, 1, Access::Write).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let body: GrantBody = match super::read_json(req.into_body(), "the grant body").await {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let (kind, reference) = match parse_grant(&body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let added = match add_capped_grant(&state, &manager, &p.id, kind, reference).await {
        Ok(added) => added,
        Err(resp) => return resp,
    };
    json_ok(
        if added {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        json!({ "kind": kind.as_str(), "ref": reference, "added": added }),
    )
}

/// Grant `kind` `reference` to principal `principal_id` (whose access the
/// caller already checked), capped at what `manager` holds right now.
/// Whether the grant is new.
pub(crate) async fn add_capped_grant(
    state: &RamaState,
    manager: &users::User,
    principal_id: &str,
    kind: GrantKind,
    reference: &str,
) -> Result<bool, Response> {
    if !manager_holds(state, manager, kind, reference).await? {
        return Err(json_error(
            StatusCode::FORBIDDEN,
            "grant_exceeds_manager",
            &format!(
                "cannot grant {} `{reference}`: you do not hold it yourself, and a manager \
                 can only grant what they hold. Ask an admin to grant it to one of your \
                 groups, or have someone who holds it make this grant.",
                kind_label(kind)
            ),
        ));
    }
    sp_db::add_grant(&state.db, principal_id, kind, reference, &manager.id)
        .await
        .map_err(internal)
}

/// POST /api/v0/system-principals/{id}/grants/revoke — remove one grant. Any
/// manager may narrow a principal; no holding check.
pub async fn revoke_grant(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let manager = require_agent_manager!(state, req);
    let p = match principal_at(&state, &req, &manager, 2, Access::Write).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let body: GrantBody = match super::read_json(req.into_body(), "the grant body").await {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let Some(kind) = GrantKind::parse(&body.kind) else {
        return bad_request(format!("`{}` is not a grant kind", body.kind));
    };
    let reference = body.reference.trim();
    match sp_db::remove_grant(&state.db, &p.id, kind, reference, &manager.id).await {
        Ok(true) => no_content(),
        Ok(false) => not_found(format!(
            "`{}` holds no {} grant `{reference}`",
            p.name,
            kind_label(kind)
        )),
        Err(err) => internal(err),
    }
}

#[derive(Deserialize)]
pub struct TokenBody {
    pub name: String,
    #[serde(default)]
    pub ttl_days: Option<i64>,
}

/// POST /api/v0/system-principals/{id}/tokens — issue a `gws_` token, capped
/// like a grant ([`manager_holds_every_grant`]). The plaintext is in this
/// response and nowhere else, ever.
pub async fn issue_token(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let manager = require_agent_manager!(state, req);
    let p = match principal_at(&state, &req, &manager, 1, Access::Write).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    if p.disabled_at.is_some() {
        return json_error(
            StatusCode::CONFLICT,
            "principal_disabled",
            &format!(
                "`{}` is disabled, so a token for it would never authenticate",
                p.name
            ),
        );
    }
    if let Err(resp) = manager_holds_every_grant(&state, &manager, &p).await {
        return resp;
    }
    let body: TokenBody = match super::read_json(req.into_body(), "the token body").await {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let name = body.name.trim();
    if name.is_empty() || name.len() > 128 {
        return bad_request("token name must be 1..=128 characters");
    }
    let ttl_days = body
        .ttl_days
        .unwrap_or(state.config().gateway.token_ttl_days)
        .clamp(1, 365 * 5);
    let (plaintext, hash) = token::mint_system();
    let expires_at = Timestamp::now() + SignedDuration::from_hours(24 * ttl_days);
    match sp_db::insert_token(&state.db, &p.id, name, &hash, expires_at, &manager.id).await {
        Ok(t) => json_ok(
            StatusCode::CREATED,
            json!({ "token": token_json(&t), "plaintext": plaintext }),
        ),
        Err(err) => internal(err),
    }
}

/// POST /api/v0/system-principals/{id}/tokens/{token_id}/revoke
pub async fn revoke_token(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let manager = require_agent_manager!(state, req);
    let p = match principal_at(&state, &req, &manager, 3, Access::Write).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let Some(token_id) = raw_path_segment(&req, 1) else {
        return bad_request("the URL is missing the token id");
    };
    match sp_db::revoke_token(&state.db, &p.id, &token_id, &manager.id).await {
        Ok(true) => no_content(),
        Ok(false) => not_found(format!("`{}` has no live token `{token_id}`", p.name)),
        Err(err) => internal(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wildcard_or_unknown_kind_is_refused_before_any_lookup() {
        let body = |kind: &str, reference: &str| GrantBody {
            kind: kind.into(),
            reference: reference.into(),
        };
        assert!(parse_grant(&body("tool", "*")).is_err());
        assert!(parse_grant(&body("skill", "brand*")).is_err());
        assert!(parse_grant(&body("pool", "chat")).is_err());
        assert!(parse_grant(&body("tool", "  ")).is_err());
        let ok = body("model", " qwen ");
        assert_eq!(parse_grant(&ok).unwrap(), (GrantKind::Model, "qwen"));
    }

    #[test]
    fn every_grant_kind_has_a_label() {
        for kind in GrantKind::ALL {
            assert!(!kind_label(kind).is_empty());
        }
    }
}
