// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Who may act on an agent (`docs/agents.md` §2): the one rule behind the
//! `/api/v0/agents` routes, the inbox's manager standing and the
//! `a2a_caller` grant.
//!
//! An admin holds `write` on every agent without a share, so an agent whose
//! last writer left can always be recovered. Anyone else needs the
//! agent-management permission *and* a share: a share takes effect only for
//! a holder of the permission, so a manager who loses it loses every agent
//! with it, whatever shares are left behind. Responders are no access level
//! here; the inbox adds them on top.

use aiplane_agents::db::agents::{self as agents_db, Access};
use aiplane_core::server::db::DbError;

use crate::rama_server::state::RamaState;

/// The access a person in `groups` holds on agent `agent_id` today: `write`
/// for an admin, otherwise their strongest share while they hold the
/// agent-management permission, otherwise none.
pub async fn effective_access(
    state: &RamaState,
    agent_id: &str,
    user_id: &str,
    groups: &[String],
) -> Result<Option<Access>, DbError> {
    if state.rbac.is_admin(groups) {
        return Ok(Some(Access::Write));
    }
    if !state.rbac.can_manage_agents(groups) {
        return Ok(None);
    }
    agents_db::access_for(&state.db, agent_id, user_id, groups).await
}
