// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The call chain of an agent run: agent → sub-agent → … → tool.
//!
//! An agent run executes as the agent's own system principal. When it calls a
//! sub-agent, that one runs as *its* principal, and so on. Every tool call
//! made anywhere in that tree is checked against the innermost principal's
//! grants and audited with the whole chain, so an audit row answers "which
//! agent, called through which agents, for which visitor". See
//! `docs/agents.md` §3 "The call chain".
//!
//! It lives here, beside [`crate::server::principal`], because the audit and
//! usage rows that serialize it are written from this crate.

use serde::Serialize;

use crate::server::principal::SystemPrincipal;

/// How many agents may be nested, the main agent included. Checked when a
/// spec is validated and again here, at run time.
pub const MAX_DEPTH: usize = 3;

/// One agent in the chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Frame {
    /// `system_principals.id`; an agent is keyed by its principal.
    pub principal_id: String,
    /// The principal's slug, so an audit row reads without a join and
    /// survives the principal's deletion.
    pub name: String,
    /// The agent version this frame executes. `None` for a principal that is
    /// not an agent with a published spec (a headless CI run).
    pub version: Option<i64>,
    /// Where the calling agent dispatched this one. `None` on the main agent.
    pub via: Option<CallSite>,
}

/// The tool call in a parent run that started a sub-agent run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CallSite {
    pub turn_id: String,
    pub tool_call_id: String,
}

impl Frame {
    pub fn for_principal(principal: &SystemPrincipal, version: Option<i64>) -> Self {
        Self {
            principal_id: principal.id.clone(),
            name: principal.name.clone(),
            version,
            via: None,
        }
    }

    pub fn called_from(mut self, via: CallSite) -> Self {
        self.via = Some(via);
        self
    }
}

/// The chain of one run. Never empty: [`RunChain::root`] starts it with the
/// main agent, and [`RunChain::enter`] only ever appends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunChain {
    /// The conversation the run tree started in (the visitor's).
    pub root_session: String,
    /// The visitor session behind the root conversation. A visitor is not a
    /// principal; it rides along for audit and limits.
    pub visitor_id: Option<String>,
    frames: Vec<Frame>,
}

/// Why a sub-agent run could not be started from a chain.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EnterError {
    #[error(
        "starting sub-agent `{sub_agent}` would nest {depth} agents deep, and at most \
         {MAX_DEPTH} are allowed; change the agent's routes so this sub-agent is not reached \
         through that many others"
    )]
    TooDeep { sub_agent: String, depth: usize },
    #[error(
        "sub-agent `{sub_agent}` is already running in this call chain ({path}); an agent \
         cannot call itself, directly or through others — change the routes so they do not \
         loop back"
    )]
    Cycle { sub_agent: String, path: String },
}

impl RunChain {
    pub fn root(root_session: impl Into<String>, visitor_id: Option<String>, agent: Frame) -> Self {
        Self {
            root_session: root_session.into(),
            visitor_id,
            frames: vec![agent],
        }
    }

    /// The chain of a sub-agent run started from this one. Refuses a fourth
    /// level and an agent that is already in the chain.
    pub fn enter(&self, sub_agent: Frame) -> Result<Self, EnterError> {
        if self
            .frames
            .iter()
            .any(|f| f.principal_id == sub_agent.principal_id)
        {
            let path: Vec<&str> = self
                .frames
                .iter()
                .map(|f| f.name.as_str())
                .chain([sub_agent.name.as_str()])
                .collect();
            return Err(EnterError::Cycle {
                path: path.join(" → "),
                sub_agent: sub_agent.name,
            });
        }
        let depth = self.frames.len() + 1;
        if depth > MAX_DEPTH {
            return Err(EnterError::TooDeep {
                sub_agent: sub_agent.name,
                depth,
            });
        }
        let mut child = self.clone();
        child.frames.push(sub_agent);
        Ok(child)
    }

    /// Main agent first, the running one last.
    pub fn frames(&self) -> &[Frame] {
        &self.frames
    }

    /// The agent running now, whose principal acts and is checked.
    pub fn current(&self) -> &Frame {
        self.frames.last().expect("a run chain is never empty")
    }

    /// The main agent at the root of the tree.
    pub fn agent(&self) -> &Frame {
        &self.frames[0]
    }

    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    /// The serialized form stored in audit rows.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("a run chain serializes")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::server::principal::GrantSet;

    fn agent(id: &str, name: &str) -> SystemPrincipal {
        SystemPrincipal {
            id: id.into(),
            name: name.into(),
            grants: Arc::new(GrantSet::default()),
        }
    }

    fn main_run() -> RunChain {
        RunChain::root(
            "s-visitor",
            Some("v-1".into()),
            Frame::for_principal(&agent("p-main", "support-website"), Some(4)),
        )
    }

    fn site(turn: &str) -> CallSite {
        CallSite {
            turn_id: turn.into(),
            tool_call_id: format!("call-{turn}"),
        }
    }

    #[test]
    fn a_root_chain_is_the_main_agent_alone() {
        let chain = main_run();
        assert_eq!(chain.depth(), 1);
        assert_eq!(chain.current(), chain.agent());
        assert_eq!(chain.current().name, "support-website");
        assert_eq!(chain.current().version, Some(4));
        assert_eq!(chain.current().via, None);
    }

    #[test]
    fn entering_a_sub_agent_appends_it_and_leaves_the_parent_unchanged() {
        let parent = main_run();
        let child = parent
            .enter(
                Frame::for_principal(&agent("p-bill", "billing"), Some(1)).called_from(site("t1")),
            )
            .unwrap();
        assert_eq!(parent.depth(), 1);
        assert_eq!(child.depth(), 2);
        assert_eq!(child.agent().name, "support-website");
        assert_eq!(child.current().principal_id, "p-bill");
        assert_eq!(child.current().via, Some(site("t1")));
        assert_eq!(child.root_session, "s-visitor");
        assert_eq!(child.visitor_id.as_deref(), Some("v-1"));
    }

    #[test]
    fn nesting_is_capped_at_three_agents() {
        let two = main_run()
            .enter(Frame::for_principal(&agent("p2", "billing"), None))
            .unwrap();
        let three = two
            .enter(Frame::for_principal(&agent("p3", "refunds"), None))
            .unwrap();
        assert_eq!(three.depth(), MAX_DEPTH);
        let err = three
            .enter(Frame::for_principal(&agent("p4", "ledger"), None))
            .unwrap_err();
        assert_eq!(
            err,
            EnterError::TooDeep {
                sub_agent: "ledger".into(),
                depth: 4
            }
        );
        assert!(err.to_string().contains("at most 3"), "{err}");
    }

    #[test]
    fn an_agent_already_in_the_chain_cannot_be_entered_again() {
        let two = main_run()
            .enter(Frame::for_principal(&agent("p-bill", "billing"), None))
            .unwrap();
        let err = two
            .enter(Frame::for_principal(
                &agent("p-main", "support-website"),
                None,
            ))
            .unwrap_err();
        assert_eq!(
            err,
            EnterError::Cycle {
                sub_agent: "support-website".into(),
                path: "support-website → billing → support-website".into(),
            }
        );
        assert!(err.to_string().contains("already running"), "{err}");
    }

    #[test]
    fn the_serialized_chain_names_every_agent_and_the_visitor() {
        let chain = main_run()
            .enter(
                Frame::for_principal(&agent("p-bill", "billing"), Some(2)).called_from(site("t9")),
            )
            .unwrap();
        assert_eq!(
            chain.to_json(),
            serde_json::json!({
                "root_session": "s-visitor",
                "visitor_id": "v-1",
                "frames": [
                    {"principal_id": "p-main", "name": "support-website", "version": 4, "via": null},
                    {"principal_id": "p-bill", "name": "billing", "version": 2,
                     "via": {"turn_id": "t9", "tool_call_id": "call-t9"}},
                ],
            })
        );
    }
}
