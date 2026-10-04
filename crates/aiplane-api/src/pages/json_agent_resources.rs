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
//! `models` lists, per kind (chat, transcription, speech), the models the
//! caller may use — the chat picker's list (`server::model_choices`), minus
//! any automatic route whose candidates or selector they may not use, since
//! granting the route would hand those on.
//!
//! `defaults` names the gateway's default model of each kind (Models &
//! routing → Default models): what an agent's unset model key runs on.
//! Whether the caller may grant it is whether `models` lists it.

use std::sync::Arc;

use rama::http::service::web::extract::State;
use rama::http::{Request, Response, StatusCode};
use serde_json::{Value, json};

use super::json_principals::require_agent_manager;
use super::{internal, json_ok};
use aiplane_core::server::db::users::User;
use aiplane_core::server::db::{mcp_catalog, rag as rag_db};
use aiplane_core::server::feature_defaults::Feature;
use aiplane_core::server::upstreams::PoolKind;
use aiplane_runtime::agents::assist::Ability;
use aiplane_runtime::rama_server::state::RamaState;
use aiplane_runtime::server::model_choices::{self, ModelChoice, gateway_default};

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

/// The models of `kind` `user` may grant: the ones they may use, an
/// automatic route only when they may use every model it reaches.
pub(super) async fn grantable_models(
    state: &RamaState,
    user: &User,
    kind: PoolKind,
) -> Vec<ModelChoice> {
    let access = state.pool_access_for(&user.roles);
    let mut choices = model_choices::offered(state, kind, &access).await;
    choices.retain(ModelChoice::grantable);
    choices
}

/// The chat models `user` may grant, by name.
pub(super) async fn grantable_chat_models(state: &RamaState, user: &User) -> Vec<String> {
    grantable_models(state, user, PoolKind::Chat)
        .await
        .into_iter()
        .map(|c| c.id)
        .collect()
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
    let mut models = serde_json::Map::new();
    for (key, kind) in [
        ("chat", PoolKind::Chat),
        ("transcription", PoolKind::Transcription),
        ("speech", PoolKind::Speech),
    ] {
        let listed: Vec<Value> = grantable_models(state, user, kind)
            .await
            .into_iter()
            .map(|c| json!({ "id": c.id, "gdpr": c.compliance.gdpr, "nda": c.compliance.nda }))
            .collect();
        models.insert(key.into(), Value::Array(listed));
    }

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

    let defaults = json!({
        "chat": gateway_default(state, Feature::Chat).await,
        "transcription": gateway_default(state, Feature::Transcription).await,
        "speech": gateway_default(state, Feature::Speech).await,
    });

    Ok(json!({
        "models": models,
        "defaults": defaults,
        "tools": tools,
        "connectors": connectors,
        "skills": skills,
        "rag_collections": rag_collections,
    }))
}
