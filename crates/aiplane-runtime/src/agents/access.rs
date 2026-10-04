// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Who may act on an agent (`docs/agents.md` §2): the one rule behind the
//! `/api/v0/agents` routes, the inbox's standing and the `a2a_caller` grant.
//!
//! An admin holds `write` on every agent without a share, so an agent whose
//! last writer left can always be recovered. Anyone else needs a share. A
//! `read` or `write` share takes effect only for a holder of the
//! agent-management permission, so a manager who loses it loses every agent
//! with it, whatever shares are left behind. A `respond` share needs no such
//! permission: it answers the agent's pending inbox items and shows nothing
//! else, and it is the only level that survives without the permission.

use aiplane_agents::db::agents::{self as agents_db, Access};
use aiplane_core::server::db::DbError;

use crate::rama_server::state::RamaState;

/// The access a person in `groups` holds on agent `agent_id` today: `write`
/// for an admin, otherwise their strongest share that takes effect for them.
pub async fn effective_access(
    state: &RamaState,
    agent_id: &str,
    user_id: &str,
    groups: &[String],
) -> Result<Option<Access>, DbError> {
    if state.rbac.is_admin(groups) {
        return Ok(Some(Access::Write));
    }
    let manager = state.rbac.can_manage_agents(groups);
    Ok(agents_db::shares_held(&state.db, agent_id, user_id, groups)
        .await?
        .into_iter()
        .filter(|a| manager || !a.needs_agent_manager())
        .max())
}
