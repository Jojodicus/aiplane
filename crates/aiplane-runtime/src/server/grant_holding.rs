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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aiplane_agents::db::agents::{self as agents_db, Access};
use aiplane_core::server::db::{DbError, mcp_catalog, rag as rag_db, users};
use aiplane_core::server::principal::{GrantKind, GrantSet, SystemPrincipal};
use aiplane_core::server::upstreams::{PoolAccess, PoolKind};

use crate::rama_server::state::RamaState;
use crate::server::model_choices::{self, ModelChoice};
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
        GrantKind::Model => {
            if offers(state, &PoolAccess::all(), reference).await.is_none() {
                return Err(missing(format!("model `{reference}`")));
            }
            offers(state, &state.pool_access_for(&user.roles), reference)
                .await
                .is_some_and(|choice| choice.grantable())
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

/// The choice named `model` among the models of every kind an agent can use
/// that `access` may pick (`server::model_choices`).
async fn offers(state: &RamaState, access: &PoolAccess, model: &str) -> Option<ModelChoice> {
    for kind in GRANTABLE_KINDS {
        if let Some(found) = model_choices::offered(state, kind, access)
            .await
            .into_iter()
            .find(|c| c.id == model)
        {
            return Some(found);
        }
    }
    None
}

/// The kinds of model a model grant can name: what an agent runs on.
const GRANTABLE_KINDS: [PoolKind; 3] = [PoolKind::Chat, PoolKind::Transcription, PoolKind::Speech];

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

/// How long a token's capped grants are reused before they are worked out
/// again. Every in-process change that can shrink them — a login that
/// changes the minter's roles, an RBAC, settings or runtime reload, a
/// connector, a RAG collection's groups, an agent share — calls
/// [`GrantCaps::invalidate`], so the TTL only bounds what this process
/// cannot see: a skill directory edited on disk, a second gateway writing
/// the same database.
pub const CAP_TTL: Duration = Duration::from_secs(30);

/// A system token's `last_used_at` is written at most this often.
pub const TOUCH_EVERY: Duration = Duration::from_secs(60);

/// More tokens than this in either map and the stale entries are dropped.
const CAPS_PRUNE_AT: usize = 4096;

/// What [`capped_to_minter`] decided per system token, and when each token's
/// last use was written (`docs/auth.md` → "System principal tokens").
#[derive(Default)]
pub struct GrantCaps {
    capped: Mutex<CappedTokens>,
    touched: Mutex<HashMap<String, Instant>>,
}

#[derive(Default)]
struct CappedTokens {
    /// Bumped by every invalidation, so a cap worked out from what held
    /// before one is not stored after it.
    epoch: u64,
    by_token: HashMap<String, Capped>,
}

struct Capped {
    at: Instant,
    minted_by: String,
    /// The principal's own grants the cap was taken from: a grant added or
    /// removed since makes the entry miss, with no invalidation needed.
    granted: Arc<GrantSet>,
    capped: Arc<GrantSet>,
}

impl GrantCaps {
    /// Forget every cap: something the minters' rights depend on changed.
    pub fn invalidate(&self) {
        let mut caps = self.capped.lock().unwrap_or_else(|p| p.into_inner());
        caps.epoch += 1;
        caps.by_token.clear();
    }

    /// Whether `token_id`'s `last_used_at` is due a write; if so, it counts
    /// as written now.
    pub fn touch_due(&self, token_id: &str) -> bool {
        let mut touched = self.touched.lock().unwrap_or_else(|p| p.into_inner());
        if touched
            .get(token_id)
            .is_some_and(|at| at.elapsed() < TOUCH_EVERY)
        {
            return false;
        }
        if touched.len() >= CAPS_PRUNE_AT {
            touched.retain(|_, at| at.elapsed() < TOUCH_EVERY);
        }
        touched.insert(token_id.to_string(), Instant::now());
        true
    }

    fn get(
        &self,
        token_id: &str,
        minted_by: &str,
        granted: &GrantSet,
    ) -> (u64, Option<Arc<GrantSet>>) {
        let caps = self.capped.lock().unwrap_or_else(|p| p.into_inner());
        let hit = caps
            .by_token
            .get(token_id)
            .filter(|c| {
                c.at.elapsed() < CAP_TTL && c.minted_by == minted_by && *c.granted == *granted
            })
            .map(|c| c.capped.clone());
        (caps.epoch, hit)
    }

    fn put(&self, epoch: u64, token_id: &str, entry: Capped) {
        let mut caps = self.capped.lock().unwrap_or_else(|p| p.into_inner());
        if caps.epoch != epoch {
            return;
        }
        if caps.by_token.len() >= CAPS_PRUNE_AT {
            caps.by_token.retain(|_, c| c.at.elapsed() < CAP_TTL);
        }
        caps.by_token.insert(token_id.to_string(), entry);
    }
}

/// [`capped_to_minter`] for system token `token_id`, reusing the cap worked
/// out for it within [`CAP_TTL`] while the principal's grants, the minter
/// and everything [`GrantCaps::invalidate`] watches stay as they were.
pub async fn capped_for_token(
    state: &RamaState,
    token_id: &str,
    principal: SystemPrincipal,
    minted_by: &str,
) -> Result<SystemPrincipal, DbError> {
    let (epoch, hit) = state.grant_caps.get(token_id, minted_by, &principal.grants);
    if let Some(grants) = hit {
        return Ok(SystemPrincipal {
            grants,
            ..principal
        });
    }
    let granted = principal.grants.clone();
    let capped = capped_to_minter(state, principal, minted_by).await?;
    state.grant_caps.put(
        epoch,
        token_id,
        Capped {
            at: Instant::now(),
            minted_by: minted_by.to_string(),
            granted,
            capped: capped.grants.clone(),
        },
    );
    Ok(capped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(granted: &GrantSet) -> Capped {
        Capped {
            at: Instant::now(),
            minted_by: "m".into(),
            granted: Arc::new(granted.clone()),
            capped: Arc::new(GrantSet::default()),
        }
    }

    #[test]
    fn a_cap_worked_out_before_an_invalidation_is_not_kept_after_it() {
        let caps = GrantCaps::default();
        let granted = GrantSet::new([(GrantKind::Tool, "time".to_string())]);
        let (epoch, hit) = caps.get("t1", "m", &granted);
        assert!(hit.is_none());
        caps.invalidate();
        caps.put(epoch, "t1", entry(&granted));
        assert!(caps.get("t1", "m", &granted).1.is_none());

        let (epoch, _) = caps.get("t1", "m", &granted);
        caps.put(epoch, "t1", entry(&granted));
        assert!(caps.get("t1", "m", &granted).1.is_some());
        assert!(
            caps.get("t1", "m", &GrantSet::default()).1.is_none(),
            "a changed grant set misses"
        );
        assert!(caps.get("t1", "other", &granted).1.is_none());
        caps.invalidate();
        assert!(caps.get("t1", "m", &granted).1.is_none());
    }

    #[test]
    fn a_tokens_last_use_is_written_at_most_once_a_minute() {
        let caps = GrantCaps::default();
        assert!(caps.touch_due("t1"));
        assert!(!caps.touch_due("t1"));
        assert!(caps.touch_due("t2"));
    }
}
