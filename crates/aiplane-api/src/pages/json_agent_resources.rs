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
//! unavailable instead of hiding it.

use std::sync::Arc;

use rama::http::service::web::extract::State;
use rama::http::{Request, Response, StatusCode};
use serde_json::json;

use super::json_principals::require_agent_manager;
use super::{internal, json_ok};
use aiplane_core::server::db::{mcp_catalog, rag as rag_db};
use aiplane_core::server::upstreams::PoolKind;
use aiplane_runtime::rama_server::state::RamaState;

const MCP_TOOL_PREFIX: &str = aiplane_runtime::server::tools::mcp::MCP_ID_PREFIX;

pub async fn resources(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let user = match require_agent_manager(&state, &req).await {
        Ok(user) => user,
        Err(resp) => return resp,
    };
    let role_ids = state.rbac.role_ids_for(&user.roles);
    let is_admin = state.rbac.is_admin(&role_ids);

    let access = state.pool_access_for(&user.roles);
    let mut pools: Vec<String> = state
        .upstreams
        .pools()
        .into_iter()
        .filter(|p| p.kind == PoolKind::Chat && access.allows(p))
        .map(|p| p.name.clone())
        .collect();
    pools.sort();

    let grantable = state.grantable_tool_ids();
    let mut held = state.rbac.allowed_tools(&role_ids, &state.tools());
    state.expand_comfyui_tools(&mut held, &role_ids);
    let tool_ids: Vec<String> = held
        .into_iter()
        .filter(|id| !id.starts_with(MCP_TOOL_PREFIX) && grantable.contains(id))
        .collect();
    let summaries = state.tools().summaries_for(&tool_ids);
    let tools: Vec<_> = tool_ids
        .iter()
        .map(|id| {
            let known = summaries.iter().find(|s| &s.id == id);
            json!({
                "id": id,
                "name": known.map_or(id.as_str(), |s| s.name.as_str()),
                "description": known.map(|s| s.description.as_str()),
            })
        })
        .collect();

    let connectors = match mcp_catalog::list_enabled(&state.db).await {
        Ok(all) => all,
        Err(err) => return internal(err),
    };
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

    let collections = match rag_db::list_collections(&state.db).await {
        Ok(all) => all,
        Err(err) => return internal(err),
    };
    let rag_collections: Vec<_> = collections
        .into_iter()
        .filter(|c| state.rbac.resource_allowed(&role_ids, &c.allowed_groups))
        .map(|c| json!({ "id": c.id, "name": c.name }))
        .collect();

    let config = state.config();
    let agents = &config.agents;
    let tiers = json!({
        "fast": agents.pool_fast,
        "balanced": agents.pool_balanced,
        "thorough": agents.pool_thorough,
    });

    json_ok(
        StatusCode::OK,
        json!({
            "pools": pools,
            "tiers": tiers,
            "tools": tools,
            "connectors": connectors,
            "skills": skills,
            "rag_collections": rag_collections,
        }),
    )
}
