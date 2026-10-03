// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Whether a person holds what a system principal's grant hands out
//! (`docs/agents.md` §1, `docs/auth.md` → "System principal tokens").
//!
//! One rule, used twice: a manager may grant only what they hold
//! (`aiplane-api`'s `json_principals`, at grant and token-issue time), and a
//! token a non-admin manager minted carries only what that manager holds at
//! authentication time ([`capped_to_minter`], from `require_bearer`). It is
//! the same rule that decides the person's own access to each resource.

use std::sync::Arc;

use aiplane_agents::db::agents::{self as agents_db, Access};
use aiplane_core::server::db::{DbError, mcp_catalog, rag as rag_db, users};
use aiplane_core::server::principal::{GrantKind, GrantSet, SystemPrincipal};

use crate::rama_server::state::RamaState;
use crate::server::tools::mcp::MCP_ID_PREFIX;

/// Why a grant could not be judged held: the resource does not exist, the
/// reference is not one, or only an admin may grant it.
#[derive(Debug, thiserror::Error)]
pub enum HoldRefusal {
    #[error("there is no {0} on this gateway")]
    Missing(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    AdminOnly(String),
    #[error(transparent)]
    Db(#[from] DbError),
}

/// Whether `user` holds `kind` `reference` today.
pub async fn holds(
    state: &RamaState,
    user: &users::User,
    kind: GrantKind,
    reference: &str,
) -> Result<bool, HoldRefusal> {
    let role_ids = state.rbac.role_ids_for(&user.roles);
    let missing = |what: String| HoldRefusal::Missing(what);
    let held = match kind {
        GrantKind::Tool => {
            if reference.starts_with(MCP_ID_PREFIX) {
                return Err(HoldRefusal::Invalid(format!(
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
            let connector = match mcp_catalog::get(&state.db, reference).await? {
                Some(c) if c.enabled => c,
                _ => return Err(missing(format!("enabled connector `{reference}`"))),
            };
            if !connector.has_shared_identity() {
                return Err(HoldRefusal::Invalid(format!(
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
                let key = format!("{MCP_ID_PREFIX}{reference}");
                connector.allows(&role_ids, is_admin)
                    && state.mcp_grant_for(&user.roles).allows(&key, &key)
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
                return Err(HoldRefusal::Invalid(format!(
                    "a RAG collection is granted by its numeric id, not `{reference}`"
                )));
            };
            let Some(collection) = rag_db::find_collection_by_id(&state.db, id).await? else {
                return Err(missing(format!("RAG collection with id {id}")));
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
            state.pool_access_for(&user.roles).allows(&pool)
        }
        GrantKind::A2aCaller => {
            // Letting another platform call an agent is a change to that
            // agent, so it takes what changing it takes: a `write` share.
            if agents_db::get(&state.db, reference).await?.is_none() {
                return Err(missing(format!("agent `{reference}`")));
            }
            state.rbac.is_admin(&role_ids)
                || agents_db::access_for(&state.db, reference, &user.id, &role_ids)
                    .await?
                    .is_some_and(|a| a >= Access::Write)
        }
        GrantKind::A2aAgent => {
            if let Err(why) = crate::agents::a2a_client::check_card_url(reference) {
                return Err(HoldRefusal::Invalid(format!(
                    "`{reference}` cannot be granted as an A2A agent: {why}"
                )));
            }
            // An external agent is nothing a person holds, and a grant on one
            // lets an agent send visitor data off the gateway: the
            // operator's call.
            if !state.rbac.is_admin(&role_ids) {
                return Err(HoldRefusal::AdminOnly(format!(
                    "cannot grant A2A agent `{reference}`: an external agent receives what its \
                     route sends it, so only an admin may grant one. Ask an admin to make this \
                     grant."
                )));
            }
            true
        }
    };
    Ok(held)
}

/// `principal` as a token minted by user `minted_by` may use it: every grant
/// when the minter is an admin today, otherwise only the grants the minter
/// holds today — none once the minter is gone. A grant whose resource is
/// gone counts as not held.
pub async fn capped_to_minter(
    state: &RamaState,
    principal: SystemPrincipal,
    minted_by: &str,
) -> Result<SystemPrincipal, DbError> {
    let Some(minter) = users::find_by_id(&state.db, minted_by).await? else {
        return Ok(SystemPrincipal {
            grants: Arc::new(GrantSet::default()),
            ..principal
        });
    };
    if state.rbac.is_admin(&state.rbac.role_ids_for(&minter.roles)) {
        return Ok(principal);
    }
    let mut kept = Vec::new();
    for (kind, reference) in principal.grants.iter() {
        match holds(state, &minter, kind, reference).await {
            Ok(true) => kept.push((kind, reference.to_string())),
            Ok(false)
            | Err(HoldRefusal::Missing(_))
            | Err(HoldRefusal::Invalid(_))
            | Err(HoldRefusal::AdminOnly(_)) => {}
            Err(HoldRefusal::Db(err)) => return Err(err),
        }
    }
    Ok(SystemPrincipal {
        grants: Arc::new(GrantSet::new(kept)),
        ..principal
    })
}
