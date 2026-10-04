// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

use std::collections::HashMap;
use std::sync::Arc;

use jiff::Timestamp;
use rama::http::service::web::extract::State;
use rama::http::{Request, Response, StatusCode};

use super::{internal, json_ok};
use aiplane_core::server::db::{
    DbError, limits, model_defaults, token_models, token_tool_prefs, tokens, usage, user_mcp,
};
use aiplane_core::server::limits::LimitStatus;
use aiplane_core::server::upstreams::PoolAccess;
use aiplane_runtime::rama_server::state::RamaState;

/// The models a token can be limited to, each with what the person choosing
/// needs to weigh: its kind, where its data goes, and its price. An alias is
/// priced as the model it resolves to, since that is what it is metered as.
pub(crate) async fn model_catalog(
    state: &RamaState,
    access: &PoolAccess,
) -> Result<Vec<serde_json::Value>, DbError> {
    let prices = model_defaults::all_prices(&state.db).await?;
    Ok(state
        .upstreams
        .model_catalog_for(access)
        .into_iter()
        .map(|model| {
            let price = prices.get(model.alias_of.as_deref().unwrap_or(&model.id));
            serde_json::json!({
                "id": model.id,
                "kind": model.kind.as_str(),
                "gdpr": model.compliance.gdpr,
                "nda": model.compliance.nda,
                "alias_of": model.alias_of,
                "price": price.map(|price| serde_json::json!({
                    "input": price.input_price,
                    "output": price.output_price,
                    "unit": price.pricing_unit.as_str(),
                })),
            })
        })
        .collect())
}

/// One in-force limit with what has been spent against it, as `/usage` and
/// the token editor both draw it.
pub fn limit_status_json(status: &LimitStatus) -> serde_json::Value {
    serde_json::json!({
        "model": status.model,
        "dimension": status.dimension.as_str(),
        "window": status.window.as_str(),
        "limit": status.limit,
        "used": status.used,
        "refreshes_at": status.refreshes_at.to_string(),
    })
}

pub async fn details(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let (session, user) = require_session_json!(state, req);
    let token_rows = match tokens::list_for_user(&state.db, &user.id).await {
        Ok(rows) => rows,
        Err(err) => return internal(err),
    };
    let model_lists = match token_models::lists_for_user(&state.db, &user.id).await {
        Ok(lists) => lists,
        Err(err) => return internal(err),
    };
    let mut quotas: HashMap<String, Vec<limits::LimitRule>> = HashMap::new();
    match limits::for_tokens_of_user(&state.db, &user.id).await {
        Ok(rules) => {
            for rule in rules {
                quotas
                    .entry(rule.subject_id.clone())
                    .or_default()
                    .push(rule);
            }
        }
        Err(err) => return internal(err),
    }

    let timezone = session
        .timezone
        .as_deref()
        .or(user.timezone.as_deref())
        .unwrap_or("UTC");
    let now = Timestamp::now();
    let usage_by_token = if state.usage.is_enabled() {
        let bounds = usage::period_bounds(usage::Period::ThisMonth, timezone, now);
        match usage::by_token(
            &state.db,
            bounds,
            Some(&user.id),
            state.config().usage.retention_days,
            now,
        )
        .await
        {
            Ok(rows) => rows.into_iter().map(|row| (row.key.clone(), row)).collect(),
            Err(err) => return internal(err),
        }
    } else {
        HashMap::new()
    };

    let capabilities =
        super::tool_toggles::token_capabilities_for_user(&state, &user.roles, &user.id).await;
    let mut token_details = Vec::with_capacity(token_rows.len());
    for token in token_rows {
        let lists = model_lists.get(&token.id).cloned().unwrap_or_default();
        let tool_states = match token_tool_prefs::states_for_token(&state.db, &token.id).await {
            Ok(states) => states
                .into_iter()
                .map(|(key, mode)| {
                    (
                        key,
                        match mode {
                            1 => "on",
                            2 => "auto",
                            _ => "off",
                        },
                    )
                })
                .collect::<HashMap<_, _>>(),
            Err(err) => return internal(err),
        };
        let mcp_allow = match user_mcp::token_ask_policy(&state.db, &token.id, "*").await {
            Ok(user_mcp::AskOverApi::Allow) => true,
            Ok(user_mcp::AskOverApi::Block) => false,
            Err(err) => return internal(err),
        };
        let token_quotas = quotas.remove(&token.id).unwrap_or_default();
        let quota_status = state.enforcer.token_statuses(&token.id).await;
        let usage = usage_by_token.get(&token.id);
        token_details.push(serde_json::json!({
            "id": token.id,
            "name": token.name,
            "created_at": token.created_at,
            "last_used_at": token.last_used_at,
            "expires_at": token.expires_at,
            "revoked": token.revoked_at.is_some(),
            "tools_enabled": token.tools_enabled,
            "tool_states": tool_states,
            "owner_models": lists.owner,
            "admin_models": lists.admin,
            "mcp_allow": mcp_allow,
            "quotas": token_quotas.into_iter().map(|rule| serde_json::json!({
                "id": rule.id,
                "model": rule.model,
                "dimension": rule.dimension.as_str(),
                "window": rule.window.as_str(),
                "value": rule.value,
                "managed_by": rule.managed_by.as_str(),
            })).collect::<Vec<_>>(),
            "quota_status": quota_status.iter().map(limit_status_json).collect::<Vec<_>>(),
            "usage": usage.map(|row| serde_json::json!({
                "requests": row.requests,
                "tokens": row.total_tokens,
                "cost": row.cost,
            })),
        }));
    }
    let role_ids = state.rbac.role_ids_for(&user.roles);
    let models = match model_catalog(&state, &state.pool_access_for(&user.roles)).await {
        Ok(models) => models,
        Err(err) => return internal(err),
    };
    let owner_limits = state.enforcer.statuses(&user.id, &role_ids).await;
    json_ok(
        StatusCode::OK,
        serde_json::json!({
            "tokens": token_details,
            "capabilities": capabilities,
            "models": models,
            "owner_limits": owner_limits.iter().map(limit_status_json).collect::<Vec<_>>(),
            "usage_enabled": state.usage.is_enabled(),
        "currency": state.config().usage.currency,
        "push_enabled": state.push.is_some(),
        "timezone": timezone,
        "account": {
                "email": user.email,
                "user_id": user.id,
                "oidc_roles": user.roles,
                "rbac_roles": role_ids,
            },
        }),
    )
}
