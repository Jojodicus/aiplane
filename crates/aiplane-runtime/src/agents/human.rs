// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Handing a conversation to a person (`docs/agent-hil.md`).
//!
//! A `human` route is a target like a sub-agent, reached two ways: the main
//! agent calls `request_human(question)`, offered whenever the spec has a
//! `human` route, or `forward_request` picks one. Either way the gate of the
//! route must be open, and the handoff is a pause of kind `human_answer`:
//!
//! - **The handoff** is the question and a small context built from state —
//!   the visitor's last message and the slots as the model sees them (never a
//!   verifier's or the host's value), each under the `label` its manager gave
//!   it (`state.<slot>.description`) when it has one. The transcript goes along only when the
//!   route sets `transcript: true`. It is stored with the pause and shown in
//!   the inbox; it is audited as `human_handoff` with the run chain.
//! - **The answer** goes back through the main agent: it is the call's
//!   result, and the model phrases it for the visitor. Chosen over passing it
//!   on verbatim because the visitor and the staff member need not share a
//!   language, the answer stays part of the conversation the model continues,
//!   and it still passes the output filter like every other answer.
//! - **Nobody answers** in the route's `timeout` (default 30 minutes): the
//!   turn ends with the catalog's `agent-human-no-answer` in the visitor's
//!   language, without another model call ([`end_unanswered`]).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use aiplane_agents::db::agent_audit::AuditKind;
use serde_json::{Value, json};
use session_core::db::{self as chat, Decision, ToolCallStatus, TurnRole, TurnStatus};
use session_core::i18n::{Lang, t};
use shared::api::ToolDef;

use super::gate::{GateInput, GateStatus};
use super::profile::RunOptions;
use super::router::RouterSpec;
use super::spec::model::{Route, RouteTarget};
use super::state::SlotStatus;
use crate::server::tools::{Tool, ToolContext, ToolError, ToolFuture};
use crate::suspend::{Suspend, SuspendRequest, tool_suspend};

pub const REQUEST_HUMAN: &str = "request_human";

/// How long a handoff waits when its route sets no `timeout`.
pub const DEFAULT_HUMAN_TIMEOUT: Duration = Duration::from_secs(30 * 60);

const MAX_QUESTION_CHARS: usize = 2_000;
const MAX_MESSAGE_CHARS: usize = 2_000;
/// Turns of transcript a route with `transcript: true` hands over.
const TRANSCRIPT_TURNS: usize = 20;

/// A `human` route as the spec configures it.
#[derive(Debug, Clone, PartialEq)]
pub struct HumanRoute {
    pub name: String,
    pub description: Option<String>,
    pub timeout: Duration,
    pub transcript: bool,
    /// `push`, `slack`, `discord`; `None` announces on every channel.
    pub notify: Option<Vec<String>>,
    /// A label the inbox shows, e.g. the team that answers.
    pub inbox: Option<String>,
}

/// Every route of `routes` (the spec's `routes`) that goes to a person.
pub fn human_routes(routes: &BTreeMap<String, Route>) -> BTreeMap<String, HumanRoute> {
    routes
        .iter()
        .filter_map(|(name, route)| {
            let RouteTarget::Human(human) = &route.target else {
                return None;
            };
            Some((
                name.clone(),
                HumanRoute {
                    name: name.clone(),
                    description: route.description.clone(),
                    timeout: human.timeout(),
                    transcript: human.transcript(),
                    notify: human.notify.clone(),
                    inbox: human.inbox.clone(),
                },
            ))
        })
        .collect()
}

/// The handoff stored with a pause, if the pause is one.
pub fn handoff_of(run_context: Option<&Value>) -> Option<&Value> {
    run_context?.get("handoff")
}

/// What a staff member's answer comes back to the model as.
pub fn answered(value: &Value) -> Value {
    json!({
        "answered": true,
        "answer": value,
        "note": "A member of staff answered the visitor's request. Pass the answer on to the \
                 visitor in your own words and in their language; do not add promises it does \
                 not make.",
    })
}

/// The context a staff member sees next to the question.
async fn context(
    ctx: &ToolContext,
    route: &HumanRoute,
    question: &str,
    spec: &RouterSpec,
) -> Result<Value, ToolError> {
    let schema = &*spec.schema;
    let session = ctx.session_id.as_deref().unwrap_or_default();
    let state = spec
        .snapshot
        .get(&ctx.db, schema, session)
        .await
        .map_err(|e| ToolError::Failed(format!("reading the conversation state: {e}")))?;
    let slots: Vec<Value> = state
        .view(schema)
        .into_iter()
        .filter(|v| v.status == SlotStatus::Set)
        .map(|v| {
            let mut slot = match v.value {
                Some(value) => json!({ "slot": v.slot, "value": value }),
                None => json!({ "slot": v.slot, "set_by": v.by }),
            };
            if let Some(label) = schema
                .slot(&v.slot)
                .and_then(|def| def.description.as_deref())
                .map(str::trim)
                .filter(|label| !label.is_empty())
            {
                slot["label"] = json!(label);
            }
            slot
        })
        .collect();
    let turns = chat::list_turns(&ctx.db, session)
        .await
        .map_err(|e| ToolError::Failed(format!("reading the conversation: {e}")))?;
    let visitor_message = turns
        .iter()
        .rev()
        .find(|t| t.turn.role == TurnRole::User)
        .and_then(|t| t.turn.user_content.as_deref())
        .map(|m| session_core::text::truncate_chars(m, MAX_MESSAGE_CHARS));
    let mut handoff = json!({
        "route": route.name,
        "question": question,
        "visitor_message": visitor_message,
        "slots": slots,
        "lang": ctx.conversation_lang().await.code(),
        "inbox": route.inbox,
        "notify": route.notify,
    });
    if route.transcript {
        let lines: Vec<Value> = turns
            .iter()
            .filter_map(|t| {
                let text = match t.turn.role {
                    TurnRole::User => t.turn.user_content.as_deref(),
                    TurnRole::Assistant if t.turn.status == TurnStatus::Completed => {
                        t.turn.content.as_deref()
                    }
                    TurnRole::Assistant => None,
                }?;
                Some(json!({
                    "role": t.turn.role,
                    "text": session_core::text::truncate_chars(text, MAX_MESSAGE_CHARS),
                }))
            })
            .collect();
        let skip = lines.len().saturating_sub(TRANSCRIPT_TURNS);
        handoff["transcript"] = Value::Array(lines.into_iter().skip(skip).collect());
    }
    Ok(handoff)
}

/// Hand the conversation to a person on `route`, or, when the call runs
/// again with their answer, return it. `via` names the tool, for the audit.
pub async fn hand_off(
    ctx: &ToolContext,
    route: &HumanRoute,
    question: &str,
    spec: &RouterSpec,
    via: &str,
) -> Result<Value, ToolError> {
    match &ctx.suspend {
        Suspend::Decided(_, Decision::Value { value }) => Ok(answered(value)),
        Suspend::Decided(_, other) => Err(ToolError::Failed(format!(
            "the request for a person was not answered (decision: {:?}).",
            other.kind()
        ))),
        Suspend::Unavailable => Err(ToolError::Failed(
            "handing the conversation to a person needs a run that can wait for them, and this \
             one cannot. Tell the visitor nobody can take over right now."
                .into(),
        )),
        Suspend::Available => {
            let question = session_core::text::truncate_chars(question.trim(), MAX_QUESTION_CHARS);
            let handoff = context(ctx, route, &question, spec).await?;
            ctx.audit(
                AuditKind::HumanHandoff,
                json!({
                    "route": route.name,
                    "via": via,
                    "timeout_secs": route.timeout.as_secs(),
                    "transcript": route.transcript,
                }),
            )
            .await;
            Ok(tool_suspend(SuspendRequest::human_answer(
                question,
                route.timeout,
                json!({ "handoff": handoff }),
            )))
        }
    }
}

/// `request_human(question)`: the main agent asks for a person.
pub struct RequestHuman {
    spec: Arc<RouterSpec>,
    humans: BTreeMap<String, HumanRoute>,
    options: RunOptions,
}

impl RequestHuman {
    /// `None` when the spec has no `human` route to request.
    pub fn new(spec: Arc<RouterSpec>, options: RunOptions) -> Option<Self> {
        let humans = human_routes(&spec.agent.routes);
        (!humans.is_empty()).then_some(Self {
            spec,
            humans,
            options,
        })
    }

    async fn request(&self, ctx: &ToolContext, question: &str) -> Result<Value, ToolError> {
        let session = ctx.session_id.as_deref().ok_or_else(|| {
            ToolError::Failed(
                "request_human only works inside an agent conversation. Do not retry.".into(),
            )
        })?;
        let state = self
            .spec
            .snapshot
            .get(&ctx.db, &self.spec.schema, session)
            .await
            .map_err(|e| ToolError::Failed(format!("reading the conversation state: {e}")))?;
        let input = GateInput {
            schema: &self.spec.schema,
            state: &state,
            now: (self.options.now)(),
        };
        let statuses = self.spec.gates.statuses(input);
        let order: &[String] = self
            .spec
            .agent
            .router
            .as_ref()
            .map_or(&[], |r| r.order.as_slice());
        let open = |name: &String| {
            statuses
                .iter()
                .any(|(n, s)| n == name && *s == GateStatus::Open)
        };
        let picked = order
            .iter()
            .filter(|n| self.humans.contains_key(*n))
            .chain(self.humans.keys())
            .find(|n| open(n));
        let Some(route) = picked.and_then(|n| self.humans.get(n)) else {
            return Ok(json!({
                "requested": false,
                "reason": "no_open_route",
                "message": "No route to a person is open yet, so nobody was asked. Collect what \
                            it is missing, then call request_human again.",
                "routes": statuses
                    .iter()
                    .filter(|(name, _)| self.humans.contains_key(name))
                    .map(|(name, status)| match status {
                        GateStatus::Closed { missing } => json!({ "route": name, "missing": missing }),
                        GateStatus::Open => json!({ "route": name }),
                    })
                    .collect::<Vec<_>>(),
            }));
        };
        hand_off(ctx, route, question, &self.spec, REQUEST_HUMAN).await
    }
}

impl Tool for RequestHuman {
    fn id(&self) -> &str {
        REQUEST_HUMAN
    }

    fn schema(&self) -> ToolDef {
        ToolDef::function(
            REQUEST_HUMAN,
            "Hand the visitor's request to a member of staff when you cannot answer it yourself \
             or the visitor asks for a person. The conversation waits for their answer, which \
             comes back to you to pass on. Write the question they should answer, without \
             personal data the visitor did not give for this purpose.",
            json!({
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "What the staff member should answer, in one or two sentences.",
                        "maxLength": MAX_QUESTION_CHARS,
                    }
                },
                "required": ["question"],
                "additionalProperties": false,
            }),
        )
    }

    fn run<'a>(&'a self, ctx: ToolContext, args: Value) -> ToolFuture<'a> {
        Box::pin(async move {
            if let Suspend::Decided(_, Decision::Value { value }) = &ctx.suspend {
                return Ok(answered(value));
            }
            let Some(map) = args.as_object() else {
                return Err(ToolError::InvalidArgs(
                    "request_human takes `{\"question\": \"…\"}`".into(),
                ));
            };
            if let Some(extra) = map.keys().find(|k| *k != "question") {
                return Err(ToolError::InvalidArgs(format!(
                    "request_human takes only `question`, not `{extra}`; whom it concerns comes \
                     from the conversation. Call it again with {{\"question\": \"…\"}}."
                )));
            }
            let question = map
                .get("question")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|q| !q.is_empty())
                .ok_or_else(|| {
                    ToolError::InvalidArgs(
                        "request_human needs a non-empty `question` for the staff member".into(),
                    )
                })?;
            self.request(&ctx, question).await
        })
    }
}

/// A handoff nobody answered in time: the waiting call is settled as
/// unanswered and the turn ends with the catalog's message in `lang`, the
/// conversation's — no model call, so the visitor reads exactly what the
/// owner's deployment ships, not a guess.
pub(crate) async fn end_unanswered(
    db: &aiplane_core::server::db::Pool,
    suspension: &chat::TurnSuspension,
    lang: Lang,
) -> Result<String, chat::DbError> {
    let message = t(lang, "agent-human-no-answer");
    chat::complete_tool_call(
        db,
        &suspension.turn_id,
        &suspension.tool_call.id,
        &Value::String("Nobody answered in time; the visitor was told so.".into()).to_string(),
        ToolCallStatus::Errored,
    )
    .await?;
    let before = chat::get_content(db, &suspension.turn_id)
        .await?
        .unwrap_or_default();
    let content = if before.trim().is_empty() {
        message.clone()
    } else {
        format!("{before}\n\n{message}")
    };
    chat::set_content(db, &suspension.turn_id, &content).await?;
    chat::finalize_turn(db, &suspension.turn_id, TurnStatus::Completed, None).await?;
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_human_route_reads_its_settings_with_defaults() {
        let spec = json!({ "routes": {
            "people": { "description": "talk to support", "when": {}, "human": {
                "timeout": "2h", "transcript": true, "notify": ["slack"], "inbox": "support" } },
            "plain": { "when": {}, "human": {} },
            "billing": { "when": {}, "agent": "x", "task": "t" }
        } });
        let spec = crate::agents::spec::AgentSpec::from_value(&spec).unwrap();
        let humans = human_routes(&spec.routes);
        assert_eq!(humans.len(), 2);
        let people = &humans["people"];
        assert_eq!(people.timeout, Duration::from_secs(7200));
        assert!(people.transcript);
        assert_eq!(people.notify.as_deref(), Some(&["slack".to_string()][..]));
        assert_eq!(people.inbox.as_deref(), Some("support"));
        let plain = &humans["plain"];
        assert_eq!(plain.timeout, DEFAULT_HUMAN_TIMEOUT);
        assert!(!plain.transcript);
        assert_eq!(plain.notify, None);
    }
}
