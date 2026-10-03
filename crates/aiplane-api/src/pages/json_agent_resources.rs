// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! `GET /api/v0/agent-resources` — what the calling manager holds and so may
//! grant to an agent, for the builder's pickers (`docs/agents.md` "What #90 built").
//!
//! It lists with the same predicates `POST /system-principals/{id}/grants`
//! checks one reference against (`json_principals::manager_holds`), so the
//! picker never offers what the grant would refuse. The grant stays the
//! authority: a resource that changed between listing and granting is still
//! refused there, with its reason.
//!
//! `tiers` names the pool behind each of the setup assistant's model choices
//! (`[agents] pool_fast/_balanced/_thorough`, set by an admin), whether or not
//! the caller holds it: the assistant says why a choice it cannot grant is
//! unavailable instead of hiding it. The choice exists only when an admin set
//! one of them; "Balanced" left unset is the chat default below.
//!
//! `defaults` names the pool (and model) the gateway's admin "Default
//! models" resolve to among the pools the caller holds — what the setup
//! preselects for a new agent's chat pool and for a voice direction switched
//! on. Being held, each is one the grant route accepts.

use std::sync::Arc;

use rama::http::service::web::extract::State;
use rama::http::{Request, Response, StatusCode};
use serde_json::{Value, json};

use super::json_principals::require_agent_manager;
use super::{internal, json_ok};
use aiplane_core::server::db::users::User;
use aiplane_core::server::db::{mcp_catalog, rag as rag_db};
use aiplane_core::server::feature_defaults::{self, Feature, PoolDefault};
use aiplane_core::server::upstreams::PoolKind;
use aiplane_runtime::agents::assist::Ability;
use aiplane_runtime::agents::defaults;
use aiplane_runtime::rama_server::state::RamaState;

const MCP_TOOL_PREFIX: &str = aiplane_runtime::server::tools::mcp::MCP_ID_PREFIX;

/// The registry tools (MCP tools aside: those come with their connector) a
/// manager in groups `role_ids` holds and may therefore grant.
pub(super) fn grantable_tools(state: &RamaState, role_ids: &[String]) -> Vec<Ability> {
    let grantable = state.grantable_tool_ids();
    let mut held = state.rbac.allowed_tools(role_ids, &state.tools());
    state.expand_comfyui_tools(&mut held, role_ids);
    let tool_ids: Vec<String> = held
        .into_iter()
        .filter(|id| !id.starts_with(MCP_TOOL_PREFIX) && grantable.contains(id))
        .collect();
    let summaries = state.tools().summaries_for(&tool_ids);
    tool_ids
        .into_iter()
        .map(|id| {
            let known = summaries.iter().find(|s| s.id == id);
            Ability {
                name: known.map_or_else(|| id.clone(), |s| s.name.clone()),
                description: known.map(|s| s.description.clone()),
                id,
            }
        })
        .collect()
}

/// The knowledge bases (RAG collections) a manager in groups `role_ids` may
/// read and may therefore grant.
pub(super) async fn grantable_collections(
    state: &RamaState,
    role_ids: &[String],
) -> Result<Vec<rag_db::Collection>, Response> {
    let collections = rag_db::list_collections(&state.db)
        .await
        .map_err(internal)?;
    Ok(collections
        .into_iter()
        .filter(|c| state.rbac.resource_allowed(role_ids, &c.allowed_groups))
        .collect())
}

/// The pools of `kind` `user` may use, by name.
fn usable_pools(state: &RamaState, user: &User, kind: PoolKind) -> Vec<String> {
    let access = state.pool_access_for(&user.roles);
    let mut names: Vec<String> = state
        .upstreams
        .pools()
        .into_iter()
        .filter(|p| p.kind == kind && access.allows(p))
        .map(|p| p.name.clone())
        .collect();
    names.sort();
    names
}

/// The chat pools `user` may use, by name.
pub(super) fn usable_chat_pools(state: &RamaState, user: &User) -> Vec<String> {
    usable_pools(state, user, PoolKind::Chat)
}

pub async fn resources(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let user = match require_agent_manager(&state, &req).await {
        Ok(user) => user,
        Err(resp) => return resp,
    };
    match resources_for(&state, &user).await {
        Ok(view) => json_ok(StatusCode::OK, view),
        Err(resp) => resp,
    }
}

/// What `user` holds and may grant, as `GET /api/v0/agent-resources`
/// answers it.
pub(super) async fn resources_for(state: &RamaState, user: &User) -> Result<Value, Response> {
    let role_ids = state.rbac.role_ids_for(&user.roles);
    let is_admin = state.rbac.is_admin(&role_ids);
    let access = state.pool_access_for(&user.roles);
    let pools = usable_chat_pools(state, user);
    // `publish.voice` names one of each for the embed widget.
    let voice_pools = json!({
        "speech": usable_pools(state, user, PoolKind::Speech),
        "transcription": usable_pools(state, user, PoolKind::Transcription),
    });

    let grantable = state.grantable_tool_ids();
    let tools: Vec<_> = grantable_tools(state, &role_ids)
        .into_iter()
        .map(|t| json!({ "id": t.id, "name": t.name, "description": t.description }))
        .collect();

    let connectors = mcp_catalog::list_enabled(&state.db)
        .await
        .map_err(internal)?;
    let mcp_grant = state.mcp_grant_for(&user.roles);
    let connectors: Vec<_> = connectors
        .into_iter()
        .filter(|c| {
            c.has_shared_identity()
                && if c.is_agent() {
                    c.grantable_by(&role_ids, is_admin)
                } else {
                    let key = format!("{MCP_TOOL_PREFIX}{}", c.key);
                    c.allows(&role_ids, is_admin) && mcp_grant.allows(&key, &key)
                }
        })
        .map(|c| {
            let prefix = format!("{MCP_TOOL_PREFIX}{}__", c.key);
            let tools: Vec<&String> = grantable
                .iter()
                .filter(|id| id.starts_with(&prefix))
                .collect();
            json!({ "key": c.key, "name": c.name, "tools": tools })
        })
        .collect();

    let skills: Vec<String> = state.skills().map_or_else(Vec::new, |store| {
        let registry = store.current();
        let mut names = state.rbac.allowed_skills(&role_ids, &registry);
        names.retain(|n| registry.names().any(|known| known == n));
        names.sort();
        names
    });

    let rag_collections: Vec<_> = grantable_collections(state, &role_ids)
        .await?
        .into_iter()
        .map(|c| json!({ "id": c.id, "name": c.name }))
        .collect();

    let chat_default = defaults::chat_pool(state, &access).await;
    let transcription_default = feature_defaults::default_pool(
        &state.db,
        &state.upstreams,
        Feature::Transcription,
        &access,
    )
    .await;
    let speech_default =
        feature_defaults::default_pool(&state.db, &state.upstreams, Feature::Speech, &access).await;
    let pool_default =
        |d: Option<PoolDefault>| d.map(|d| json!({ "pool": d.pool, "model": d.model }));
    let defaults = json!({
        "chat": pool_default(chat_default.clone()),
        "transcription": pool_default(transcription_default),
        "speech": pool_default(speech_default),
    });

    let config = state.config();
    let agents = &config.agents;
    let tiers = if [
        &agents.pool_fast,
        &agents.pool_balanced,
        &agents.pool_thorough,
    ]
    .iter()
    .any(|p| p.is_some())
    {
        json!({
            "fast": agents.pool_fast,
            "balanced": agents.pool_balanced.clone().or(chat_default.map(|d| d.pool)),
            "thorough": agents.pool_thorough,
        })
    } else {
        json!({ "fast": null, "balanced": null, "thorough": null })
    };

    Ok(json!({
        "pools": pools,
        "voice_pools": voice_pools,
        "tiers": tiers,
        "defaults": defaults,
        "tools": tools,
        "connectors": connectors,
        "skills": skills,
        "rag_collections": rag_collections,
    }))
}
