// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Who is acting: a person, or a named system principal.
//!
//! A system principal (CI, an integration, an agent) is not a user and must
//! never be treated as one. The person paths — default groups, empty
//! `allowed_groups` meaning "everyone", per-user MCP connections, memory,
//! personal skills — are all reached through [`Principal::User`]; a
//! [`Principal::System`] carries only its [`GrantSet`], and every check made
//! for it is "is this exact resource granted". See `docs/agents.md` → "Principals".

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Which table a subject id points into. Stored on usage and audit rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrincipalKind {
    User,
    System,
}

impl PrincipalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::System => "system",
        }
    }
}

/// What one `principal_grants` row grants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GrantKind {
    /// A registry tool id (or a loaded `comfyui_<id>` workflow).
    Tool,
    /// An MCP connector key; grants every tool that connector exposes.
    Connector,
    /// A global (operator-uploaded) skill name.
    Skill,
    /// A RAG collection id.
    RagCollection,
    /// A model the gateway serves, by the name a person picks it by: a
    /// model id, a backend alias or an automatic-route alias.
    Model,
    /// An agent id (`system_principals.id`) this principal may call over
    /// A2A (`docs/agent-a2a.md` → "Serving an agent over A2A"). It grants nothing else.
    A2aCaller,
    /// An external A2A agent, by its agent card URL (`docs/agent-a2a.md` → "External agents as route targets").
    A2aAgent,
}

impl GrantKind {
    pub const ALL: [GrantKind; 7] = [
        Self::Tool,
        Self::Connector,
        Self::Skill,
        Self::RagCollection,
        Self::Model,
        Self::A2aCaller,
        Self::A2aAgent,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Connector => "connector",
            Self::Skill => "skill",
            Self::RagCollection => "rag_collection",
            Self::Model => "model",
            Self::A2aCaller => "a2a_caller",
            Self::A2aAgent => "a2a_agent",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == s)
    }
}

/// Everything a system principal may use, loaded from `principal_grants`.
/// There is no wildcard: a ref is either listed or it is not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GrantSet {
    grants: BTreeSet<(GrantKind, String)>,
    /// The pools a `model` grant routes through: those the granting manager
    /// could use for it at grant time. A model grant without an entry is not
    /// narrowed to pools (an admin's grant).
    model_pools: BTreeMap<String, BTreeSet<String>>,
}

impl GrantSet {
    pub fn new(grants: impl IntoIterator<Item = (GrantKind, String)>) -> Self {
        Self {
            grants: grants.into_iter().collect(),
            model_pools: BTreeMap::new(),
        }
    }

    /// Narrow `model` grants to pools: each `(model, pools)` routes only
    /// through those pools.
    #[must_use]
    pub fn with_model_pools(
        mut self,
        pools: impl IntoIterator<Item = (String, BTreeSet<String>)>,
    ) -> Self {
        self.model_pools.extend(pools);
        self
    }

    /// The pools a granted `model` routes through; `None` when it is not
    /// narrowed to pools.
    pub fn model_pools(&self, model: &str) -> Option<&BTreeSet<String>> {
        self.model_pools.get(model)
    }

    pub fn has(&self, kind: GrantKind, reference: &str) -> bool {
        self.grants.contains(&(kind, reference.to_string()))
    }

    /// The granted refs of one kind, sorted.
    pub fn refs(&self, kind: GrantKind) -> impl Iterator<Item = &str> {
        self.grants
            .iter()
            .filter(move |(k, _)| *k == kind)
            .map(|(_, r)| r.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.grants.is_empty()
    }

    /// Every grant, sorted by kind and ref.
    pub fn iter(&self) -> impl Iterator<Item = (GrantKind, &str)> {
        self.grants.iter().map(|(k, r)| (*k, r.as_str()))
    }
}

/// A named system principal, resolved for one request or run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemPrincipal {
    pub id: String,
    /// The slug shown in usage and audit (`support-website`).
    pub name: String,
    pub grants: Arc<GrantSet>,
}

/// The acting identity of a request, tool call or run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    /// A person: their `users.id` and raw OIDC group claims.
    User {
        id: String,
        roles: Vec<String>,
    },
    System(SystemPrincipal),
}

impl Principal {
    /// Present only for a person. Tools that act *for* a person (memory,
    /// notifications, scheduled actions, location, browser control) refuse
    /// when this is `None`.
    pub fn user_id(&self) -> Option<&str> {
        match self {
            Self::User { id, .. } => Some(id),
            Self::System(_) => None,
        }
    }

    /// The person's raw OIDC claims. `None` for a system principal, on
    /// purpose: resolving an empty claim list through RBAC still yields every
    /// default group, which is exactly the inheritance a principal must not
    /// get.
    pub fn user_roles(&self) -> Option<&[String]> {
        match self {
            Self::User { roles, .. } => Some(roles),
            Self::System(_) => None,
        }
    }

    /// Stable id for scoping rows and attribution: `users.id` or
    /// `system_principals.id`.
    pub fn subject_id(&self) -> &str {
        match self {
            Self::User { id, .. } => id,
            Self::System(sp) => &sp.id,
        }
    }

    pub fn kind(&self) -> PrincipalKind {
        match self {
            Self::User { .. } => PrincipalKind::User,
            Self::System(_) => PrincipalKind::System,
        }
    }

    pub fn system(&self) -> Option<&SystemPrincipal> {
        match self {
            Self::User { .. } => None,
            Self::System(sp) => Some(sp),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system(grants: GrantSet) -> Principal {
        Principal::System(SystemPrincipal {
            id: "p1".into(),
            name: "ci".into(),
            grants: Arc::new(grants),
        })
    }

    #[test]
    fn grant_kind_round_trips_through_its_column_value() {
        for kind in GrantKind::ALL {
            assert_eq!(GrantKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(GrantKind::parse("pool"), None);
        assert_eq!(GrantKind::parse("*"), None);
    }

    #[test]
    fn calling_an_agent_over_a2a_is_a_grant_of_its_own_kind_naming_the_agent() {
        assert_eq!(GrantKind::parse("a2a_caller"), Some(GrantKind::A2aCaller));
        let grants = GrantSet::new([(GrantKind::A2aCaller, "agent-1".to_string())]);
        assert!(grants.has(GrantKind::A2aCaller, "agent-1"));
        assert!(!grants.has(GrantKind::A2aCaller, "agent-2"));
        assert!(
            !grants.has(GrantKind::Tool, "agent-1"),
            "a caller grant unlocks nothing else"
        );
    }

    #[test]
    fn an_empty_grant_set_grants_nothing() {
        let grants = GrantSet::default();
        assert!(grants.is_empty());
        for kind in GrantKind::ALL {
            assert!(!grants.has(kind, "*"));
            assert_eq!(grants.refs(kind).count(), 0);
        }
    }

    #[test]
    fn a_grant_is_exact_per_kind_and_never_a_wildcard() {
        let grants = GrantSet::new([
            (GrantKind::Tool, "time".to_string()),
            (GrantKind::Model, "chat".to_string()),
            (GrantKind::Tool, "*".to_string()),
        ]);
        assert!(grants.has(GrantKind::Tool, "time"));
        assert!(!grants.has(GrantKind::Model, "time"));
        assert!(!grants.has(GrantKind::Tool, "echo"));
        assert_eq!(
            grants.refs(GrantKind::Tool).collect::<Vec<_>>(),
            ["*", "time"]
        );
        assert_eq!(grants.refs(GrantKind::Model).collect::<Vec<_>>(), ["chat"]);
    }

    #[test]
    fn a_user_has_a_user_id_and_roles() {
        let p = Principal::User {
            id: "alice".into(),
            roles: vec!["eng".into()],
        };
        assert_eq!(p.user_id(), Some("alice"));
        assert_eq!(p.user_roles(), Some(&["eng".to_string()][..]));
        assert_eq!(p.subject_id(), "alice");
        assert_eq!(p.kind(), PrincipalKind::User);
        assert!(p.system().is_none());
    }

    #[test]
    fn a_system_principal_has_no_user_id_and_no_roles() {
        let p = system(GrantSet::default());
        assert_eq!(p.user_id(), None);
        assert_eq!(p.user_roles(), None);
        assert_eq!(p.subject_id(), "p1");
        assert_eq!(p.kind(), PrincipalKind::System);
        assert_eq!(p.kind().as_str(), "system");
        assert_eq!(p.system().map(|s| s.name.as_str()), Some("ci"));
    }
}
