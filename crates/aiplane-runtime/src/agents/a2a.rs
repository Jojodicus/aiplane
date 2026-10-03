// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Serving an agent over A2A (Agent2Agent protocol v1.0, Linux Foundation;
//! `docs/agents.md` "What #102 built"): what a published spec says about it,
//! and the agent card built from that.
//!
//! The JSON-RPC endpoint itself is `aiplane-api::pages::a2a`; it runs every
//! task through the same opened-turn path as an embed visitor's message, so
//! nothing here touches the loop.

use serde_json::{Value, json};
use session_core::db::{SuspensionKind, TurnStatus};

use super::spec::{AgentSpec, MAX_A2A_SKILLS, is_skill_id};

/// The A2A protocol version this endpoint speaks (`Major.Minor`, compared
/// against the `A2A-Version` header).
pub const PROTOCOL_VERSION: &str = "1.0";

/// The `securitySchemes` key the card names the system-token bearer under.
pub const SECURITY_SCHEME: &str = "aiplaneSystemToken";

/// What the agent is called on its card: the spec's `profile.display`, else
/// the principal's display name.
pub fn display_name<'a>(spec: &'a AgentSpec, principal_display: &'a str) -> &'a str {
    spec.profile.display().unwrap_or(principal_display)
}

/// The card's skills: `publish.a2a.skills` as written, else one for the
/// agent as a whole (from its description) plus one per route that has a
/// `description` — a route without one is internal plumbing, not something
/// to advertise.
pub fn skills(spec: &AgentSpec, agent_name: &str, display: &str, description: &str) -> Vec<Value> {
    if let Some(listed) = spec.publish.a2a.as_ref().and_then(|a| a.skills.as_ref()) {
        return listed
            .iter()
            .map(|s| {
                json!({
                    "id": s.id,
                    "name": s.name,
                    "description": s.description,
                    "tags": s.tags,
                    "examples": s.examples,
                })
            })
            .collect();
    }
    let id = if is_skill_id(agent_name) {
        agent_name
    } else {
        "conversation"
    };
    let about = if description.trim().is_empty() {
        format!("Ask {display} in plain language; it answers in text.")
    } else {
        description.to_string()
    };
    let mut out = vec![json!({
        "id": id,
        "name": display,
        "description": about,
        "tags": spec.routes.keys().collect::<Vec<_>>(),
        "examples": [],
    })];
    for (route, def) in &spec.routes {
        let Some(text) = def.description.as_deref().filter(|d| !d.trim().is_empty()) else {
            continue;
        };
        if out.len() >= MAX_A2A_SKILLS || route == id {
            break;
        }
        out.push(json!({
            "id": route,
            "name": route.replace('_', " "),
            "description": text,
            "tags": [route],
            "examples": [],
        }));
    }
    out
}

/// The facts an agent card is built from.
pub struct CardFacts<'a> {
    pub agent_name: &'a str,
    pub principal_display: &'a str,
    pub description: &'a str,
    pub live_version: i64,
    /// The live spec.
    pub spec: &'a AgentSpec,
    /// Absolute URL of the agent's JSON-RPC endpoint.
    pub endpoint: &'a str,
}

/// The agent card (A2A v1.0 `AgentCard`, ProtoJSON: camelCase fields).
/// Streaming is offered (`SendStreamingMessage`); push notifications and an
/// extended card are not. The only security scheme is an HTTP bearer: a
/// `gws_` system token granted `a2a_caller` on this agent.
pub fn agent_card(f: &CardFacts<'_>) -> Value {
    let display = display_name(f.spec, f.principal_display);
    let description = if f.description.trim().is_empty() {
        format!("{display}, an agent served by croit AIplane.")
    } else {
        f.description.to_string()
    };
    json!({
        "name": display,
        "description": description,
        "supportedInterfaces": [{
            "url": f.endpoint,
            "protocolBinding": "JSONRPC",
            "protocolVersion": PROTOCOL_VERSION,
        }],
        "version": f.live_version.to_string(),
        "capabilities": {
            "streaming": true,
            "pushNotifications": false,
            "extendedAgentCard": false,
        },
        "securitySchemes": {
            SECURITY_SCHEME: {
                "httpAuthSecurityScheme": {
                    "scheme": "Bearer",
                    "bearerFormat": "gws_",
                    "description": "An AIplane system token (gws_…) whose principal holds the \
                                    `a2a_caller` grant for this agent.",
                }
            }
        },
        "securityRequirements": [{ "schemes": { SECURITY_SCHEME: { "list": [] } } }],
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"],
        "skills": skills(f.spec, f.agent_name, display, f.description),
    })
}

/// A2A `TaskState`, as its ProtoJSON enum name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Working,
    InputRequired,
    Completed,
    Failed,
    Canceled,
}

impl TaskState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Working => "TASK_STATE_WORKING",
            Self::InputRequired => "TASK_STATE_INPUT_REQUIRED",
            Self::Completed => "TASK_STATE_COMPLETED",
            Self::Failed => "TASK_STATE_FAILED",
            Self::Canceled => "TASK_STATE_CANCELED",
        }
    }

    /// Completed, failed, canceled: the task takes no further message.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Canceled)
    }

    /// The state of the task whose agent turn has `status`. A turn still
    /// held by its runner is working even when its row is terminal: the
    /// output filter has not ruled on the answer yet.
    pub fn of(status: TurnStatus, held: bool) -> Self {
        match status {
            _ if held => Self::Working,
            TurnStatus::InProgress => Self::Working,
            TurnStatus::Suspended => Self::InputRequired,
            TurnStatus::Completed => Self::Completed,
            TurnStatus::Errored => Self::Failed,
            TurnStatus::Cancelled => Self::Canceled,
        }
    }
}

/// Who answers a suspended task: the A2A caller (a secure input, such as a
/// verification code), or the agent's staff (an approval, a handoff), for
/// whom the caller can only wait.
pub fn answered_by_caller(kind: SuspensionKind) -> bool {
    kind.answered_by() == session_core::db::Answerer::Participant
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(spec: Value) -> AgentSpec {
        AgentSpec::from_value(&spec).unwrap()
    }

    fn card(spec: Value, description: &str) -> Value {
        agent_card(&CardFacts {
            agent_name: "support",
            principal_display: "Support",
            description,
            live_version: 3,
            spec: &typed(spec),
            endpoint: "https://gw.example/a2a/agents/a1",
        })
    }

    #[test]
    fn only_an_explicit_true_opts_an_agent_in() {
        let enabled = |spec: Value| typed(spec).publish.a2a_enabled();
        assert!(enabled(
            json!({ "publish": { "a2a": { "enabled": true } } })
        ));
        for off in [
            json!({}),
            json!({ "publish": {} }),
            json!({ "publish": { "a2a": { "enabled": false } } }),
        ] {
            assert!(!enabled(off.clone()), "{off}");
        }
        let quoted = json!({ "publish": { "a2a": { "enabled": "true" } } });
        assert!(
            AgentSpec::from_value(&quoted).is_err(),
            "only a boolean reads"
        );
    }

    #[test]
    fn the_card_names_the_endpoint_the_version_and_a_bearer_system_token() {
        let c = card(
            json!({ "profile": { "display": "croit Support" } }),
            "Help desk",
        );
        assert_eq!(c["name"], "croit Support");
        assert_eq!(c["description"], "Help desk");
        assert_eq!(c["version"], "3");
        assert_eq!(
            c["supportedInterfaces"],
            json!([{ "url": "https://gw.example/a2a/agents/a1",
                     "protocolBinding": "JSONRPC", "protocolVersion": "1.0" }])
        );
        assert_eq!(
            c["securitySchemes"][SECURITY_SCHEME]["httpAuthSecurityScheme"]["scheme"],
            "Bearer"
        );
        assert_eq!(
            c["securityRequirements"],
            json!([{ "schemes": { SECURITY_SCHEME: { "list": [] } } }])
        );
        assert_eq!(c["capabilities"]["streaming"], true);
        assert_eq!(c["capabilities"]["pushNotifications"], false);
        assert_eq!(c["defaultInputModes"], json!(["text/plain"]));
    }

    #[test]
    fn without_listed_skills_the_card_derives_them_from_the_description_and_described_routes() {
        let c = card(
            json!({ "routes": {
                "billing": { "description": "Invoices and payments", "when": {}, "human": {} },
                "internal": { "when": {}, "human": {} }
            } }),
            "",
        );
        let skills = c["skills"].as_array().unwrap();
        assert_eq!(skills.len(), 2, "{skills:?}");
        assert_eq!(skills[0]["id"], "support");
        assert_eq!(skills[0]["name"], "Support");
        assert_eq!(skills[0]["tags"], json!(["billing", "internal"]));
        assert!(
            skills[0]["description"]
                .as_str()
                .unwrap()
                .contains("Support")
        );
        assert_eq!(skills[1]["id"], "billing");
        assert_eq!(skills[1]["description"], "Invoices and payments");
    }

    #[test]
    fn listed_skills_replace_the_derived_ones() {
        let c = card(
            json!({ "publish": { "a2a": { "enabled": true, "skills": [
                { "id": "faq", "name": "FAQ", "description": "Product questions" }
            ] } } }),
            "x",
        );
        assert_eq!(
            c["skills"],
            json!([{ "id": "faq", "name": "FAQ", "description": "Product questions",
                     "tags": [], "examples": [] }])
        );
    }

    #[test]
    fn a_turn_maps_onto_a_task_state() {
        assert_eq!(
            TaskState::of(TurnStatus::Completed, false),
            TaskState::Completed
        );
        assert_eq!(
            TaskState::of(TurnStatus::Completed, true),
            TaskState::Working,
            "the filter has not ruled yet"
        );
        assert_eq!(
            TaskState::of(TurnStatus::Suspended, false),
            TaskState::InputRequired
        );
        assert_eq!(TaskState::of(TurnStatus::Errored, false), TaskState::Failed);
        assert_eq!(
            TaskState::of(TurnStatus::Cancelled, false),
            TaskState::Canceled
        );
        assert!(TaskState::Canceled.is_terminal());
        assert!(!TaskState::InputRequired.is_terminal());
        assert_eq!(
            TaskState::InputRequired.as_str(),
            "TASK_STATE_INPUT_REQUIRED"
        );
    }

    #[test]
    fn a_caller_answers_a_secure_input_and_never_an_approval() {
        assert!(answered_by_caller(SuspensionKind::SecureInput));
        assert!(!answered_by_caller(SuspensionKind::Approval));
        assert!(!answered_by_caller(SuspensionKind::HumanAnswer));
    }
}
