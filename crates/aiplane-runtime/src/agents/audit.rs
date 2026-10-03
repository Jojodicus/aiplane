// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Run events in `agent_audit`. Every one is best-effort: the run goes on
//! when its row cannot be written, and the failure is logged here, once.

use aiplane_agents::db::agent_audit::{self, AuditKind};
use aiplane_core::server::db::Pool;
use aiplane_core::server::run_chain::RunChain;
use serde_json::{Value, json};

use crate::server::tools::ToolContext;

/// One run event of `principal_id`; `actor_id` names the person who caused
/// it, when one did.
pub(crate) async fn record(
    db: &Pool,
    kind: AuditKind,
    principal_id: &str,
    actor_id: Option<&str>,
    chain: Option<&RunChain>,
    detail: Value,
) {
    if let Err(err) =
        agent_audit::record_run_event_by(db, kind, principal_id, actor_id, chain, detail).await
    {
        tracing::warn!(
            error = %err,
            kind = kind.as_str(),
            principal = principal_id,
            "recording an agent run event failed; the run goes on without it"
        );
    }
}

impl ToolContext {
    /// One run event of this call's principal, with its call chain, and its
    /// conversation and turn added to `detail` unless it names them already.
    pub(crate) async fn audit(&self, kind: AuditKind, mut detail: Value) {
        if let Value::Object(map) = &mut detail {
            map.entry("session_id")
                .or_insert_with(|| json!(self.session_id));
            map.entry("turn_id")
                .or_insert_with(|| json!(self.assistant_turn_id));
        }
        record(
            &self.db,
            kind,
            self.principal.subject_id(),
            None,
            self.chain(),
            detail,
        )
        .await;
    }
}
