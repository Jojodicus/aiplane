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

use std::sync::Arc;

use jiff::{SignedDuration, Timestamp};
use rama::http::service::web::extract::State;
use rama::http::{Request, Response, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{bad_request, internal, json_error, json_ok, no_content, not_found, raw_path_segment};
use aiplane_core::server::auth::token;
use aiplane_core::server::db::{
    agent_audit, mcp_catalog, rag as rag_db, system_principals as sp_db, users,
};
use aiplane_core::server::principal::GrantKind;
use aiplane_runtime::rama_server::state::RamaState;

const MCP_TOOL_PREFIX: &str = aiplane_runtime::server::tools::mcp::MCP_ID_PREFIX;

/// Session + agent-management permission, or the 401/403 to return.
async fn require_agent_manager(state: &RamaState, req: &Request) -> Result<users::User, Response> {
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

/// The principal named by the path segment `from_end` back, or the 404.
async fn principal_at(
    state: &RamaState,
    req: &Request,
    from_end: usize,
) -> Result<sp_db::PrincipalRow, Response> {
    let Some(id) = raw_path_segment(req, from_end) else {
        return Err(bad_request("the URL is missing the principal id"));
    };
    match sp_db::get(&state.db, &id).await {
        Ok(Some(p)) => Ok(p),
        Ok(None) => Err(not_found(format!("there is no system principal `{id}`"))),
        Err(err) => Err(internal(err)),
    }
}

/// GET /api/v0/system-principals
pub async fn list(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let _ = require_agent_manager!(state, req);
    let rows = match sp_db::list(&state.db).await {
        Ok(rows) => rows,
        Err(err) => return internal(err),
    };
    let mut out = Vec::with_capacity(rows.len());
    for p in &rows {
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
    let _ = require_agent_manager!(state, req);
    let p = match principal_at(&state, &req, 0).await {
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
    let p = match principal_at(&state, &req, 1).await {
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
            "`{}` is not a grant kind — use one of tool, connector, skill, rag_collection, pool",
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

/// Why `manager` cannot grant `(kind, reference)`, or `Ok` when they hold it
/// right now. The check is the one that decides the manager's own access to
/// that resource.
async fn manager_holds(
    state: &RamaState,
    manager: &users::User,
    kind: GrantKind,
    reference: &str,
) -> Result<(), Response> {
    let role_ids = state.rbac.role_ids_for(&manager.roles);
    let missing = |what: String| not_found(format!("there is no {what} on this gateway"));
    let held = match kind {
        GrantKind::Tool => {
            if reference.starts_with(MCP_TOOL_PREFIX) {
                return Err(bad_request(format!(
                    "`{reference}` is an MCP tool — grant its connector (kind `connector`) instead"
                )));
            }
            if !state.grantable_tool_ids().iter().any(|id| id == reference) {
                return Err(missing(format!("tool `{reference}`")));
            }
            let mut held = state.rbac.allowed_tools(&role_ids, &state.tools());
            state.expand_comfyui_tools(&mut held, &role_ids);
            held.iter().any(|id| id == reference)
        }
        GrantKind::Connector => {
            let connector = match mcp_catalog::get(&state.db, reference).await {
                Ok(Some(c)) if c.enabled => c,
                Ok(_) => return Err(missing(format!("enabled connector `{reference}`"))),
                Err(err) => return Err(internal(err)),
            };
            if !connector.has_shared_identity() {
                return Err(bad_request(format!(
                    "connector `{reference}` signs in as each person, so a system principal \
                     cannot use it — only connectors with the `global` or `agent` scope can be \
                     granted"
                )));
            }
            let is_admin = state.rbac.is_admin(&role_ids);
            if connector.is_agent() {
                // No person uses an agent connector, so there is no personal
                // access to cap by; its groups say who may hand it out.
                connector.grantable_by(&role_ids, is_admin)
            } else {
                let key = format!("{MCP_TOOL_PREFIX}{reference}");
                connector.allows(&role_ids, is_admin)
                    && state.mcp_grant_for(&manager.roles).allows(&key, &key)
            }
        }
        GrantKind::Skill => {
            let Some(store) = state.skills() else {
                return Err(missing(format!(
                    "skill `{reference}` (skills are not configured)"
                )));
            };
            let registry = store.current();
            if !registry.names().any(|n| n == reference) {
                return Err(missing(format!("skill `{reference}`")));
            }
            state
                .rbac
                .allowed_skills(&role_ids, &registry)
                .iter()
                .any(|n| n == reference)
        }
        GrantKind::RagCollection => {
            let Ok(id) = reference.parse::<i64>() else {
                return Err(bad_request(format!(
                    "a RAG collection is granted by its numeric id, not `{reference}`"
                )));
            };
            let collection = match rag_db::find_collection_by_id(&state.db, id).await {
                Ok(Some(c)) => c,
                Ok(None) => return Err(missing(format!("RAG collection with id {id}"))),
                Err(err) => return Err(internal(err)),
            };
            state
                .rbac
                .resource_allowed(&role_ids, &collection.allowed_groups)
        }
        GrantKind::Pool => {
            let Some(pool) = state
                .upstreams
                .pools()
                .into_iter()
                .find(|p| p.name == reference)
            else {
                return Err(missing(format!("pool `{reference}`")));
            };
            state.pool_access_for(&manager.roles).allows(&pool)
        }
    };
    if held {
        return Ok(());
    }
    Err(json_error(
        StatusCode::FORBIDDEN,
        "grant_exceeds_manager",
        &format!(
            "cannot grant {} `{reference}`: you do not hold it yourself, and a manager can only \
             grant what they hold. Ask an admin to grant it to one of your groups, or have \
             someone who holds it make this grant.",
            kind_label(kind)
        ),
    ))
}

fn kind_label(kind: GrantKind) -> &'static str {
    match kind {
        GrantKind::Tool => "tool",
        GrantKind::Connector => "connector",
        GrantKind::Skill => "skill",
        GrantKind::RagCollection => "RAG collection",
        GrantKind::Pool => "pool",
    }
}

/// POST /api/v0/system-principals/{id}/grants — add one grant, capped at
/// what the caller holds right now.
pub async fn grant(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let manager = require_agent_manager!(state, req);
    let p = match principal_at(&state, &req, 1).await {
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
    if let Err(resp) = manager_holds(&state, &manager, kind, reference).await {
        return resp;
    }
    let added = match sp_db::add_grant(&state.db, &p.id, kind, reference, &manager.id).await {
        Ok(added) => added,
        Err(err) => return internal(err),
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

/// POST /api/v0/system-principals/{id}/grants/revoke — remove one grant. Any
/// manager may narrow a principal; no holding check.
pub async fn revoke_grant(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let manager = require_agent_manager!(state, req);
    let p = match principal_at(&state, &req, 2).await {
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

/// POST /api/v0/system-principals/{id}/tokens — issue a `gws_` token. The
/// plaintext is in this response and nowhere else, ever.
pub async fn issue_token(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let manager = require_agent_manager!(state, req);
    let p = match principal_at(&state, &req, 1).await {
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
    let p = match principal_at(&state, &req, 3).await {
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
        assert!(parse_grant(&body("model", "gpt")).is_err());
        assert!(parse_grant(&body("tool", "  ")).is_err());
        let ok = body("pool", " chat ");
        assert_eq!(parse_grant(&ok).unwrap(), (GrantKind::Pool, "chat"));
    }

    #[test]
    fn every_grant_kind_has_a_label() {
        for kind in GrantKind::ALL {
            assert!(!kind_label(kind).is_empty());
        }
    }
}
