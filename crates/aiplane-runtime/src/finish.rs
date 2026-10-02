// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The completion contract for non-interactive runs.
//!
//! An interactive turn ends when the model writes text without a tool call;
//! somebody is reading, and an unfinished answer is visible as one. A run
//! nobody watches — a scheduled action, a webhook, a sub-agent — has no reader
//! to notice, so "the model stopped talking" is not a result. A run configured
//! with a [`FinishContract`] instead ends in exactly one of two ways:
//!
//! - the model calls [`FINISH_TOOL_NAME`] with a `result` that validates
//!   against the contract's schema → [`RunOutcome::Finished`];
//! - anything else stops it first (the round budget, the output ceiling, a
//!   cancel, an error) → [`RunOutcome::Incomplete`], saying why and what was
//!   done.
//!
//! The round loop that enforces this lives in `openai_driver`; this module is
//! the contract itself, kept free of the driver so the rules are testable on
//! their own. See docs/tools-rbac.md → "Finish contract".
//!
//! # Schema subset
//!
//! The schema is checked by a deliberately small validator rather than a full
//! JSON Schema implementation (see docs/dependencies.md for why no crate):
//! `type` (a name or a list of names), `properties`, `required`, `enum`,
//! `items` (one schema for every element) and `additionalProperties` (a
//! boolean). The annotations `title`, `description`, `default`, `examples` and
//! `$schema` are accepted and ignored. Any other keyword is refused when the
//! contract is built, so a schema never *looks* enforced while part of it is
//! silently skipped.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The tool name the model ends a contracted run with.
pub const FINISH_TOOL_NAME: &str = "finish";

const VALIDATING_KEYWORDS: [&str; 6] = [
    "type",
    "properties",
    "required",
    "enum",
    "items",
    "additionalProperties",
];
const ANNOTATION_KEYWORDS: [&str; 5] = ["title", "description", "default", "examples", "$schema"];
const TYPE_NAMES: [&str; 7] = [
    "object", "array", "string", "number", "integer", "boolean", "null",
];

/// The schema a run's `finish(result)` must satisfy.
#[derive(Debug, Clone, PartialEq)]
pub struct FinishContract {
    schema: Value,
}

/// A configured finish schema uses something the validator cannot enforce.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "cannot use this finish schema: at `{path}`, {problem}. The finish contract supports only \
     type, properties, required, enum, items and a boolean additionalProperties; rewrite the \
     schema with those"
)]
pub struct SchemaError {
    pub path: String,
    pub problem: String,
}

/// How a contracted run ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RunOutcome {
    /// The model called `finish` with a schema-valid result.
    Finished { result: Value },
    /// The run stopped without one.
    Incomplete {
        reason: IncompleteReason,
        /// What was done: the model's own last words when it wrote any,
        /// otherwise a gateway-written account of the rounds it spent.
        summary: String,
    },
}

/// Why a contracted run stopped without a valid `finish`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IncompleteReason {
    /// Every round the budget allows was spent.
    RoundBudgetExhausted { rounds: u32 },
    /// The run's wall-clock allowance ran out.
    SecondsExhausted { seconds: u64 },
    /// The run's token allowance (prompt + completion, summed over its rounds)
    /// ran out.
    TokensExhausted { tokens: u64 },
    /// The repeated-call guard stopped the run: the model kept making one
    /// identical call to `tool`.
    RepeatedToolCall { tool: String },
    /// The model hit its output-token ceiling mid-reply.
    OutputTruncated,
    /// Someone cancelled the run.
    Cancelled,
    /// The run failed: an upstream error, a loop abort, a crash.
    Failed { message: String },
}

impl FinishContract {
    /// Build a contract, refusing a schema the validator would only partly
    /// enforce.
    pub fn new(schema: Value) -> Result<Self, SchemaError> {
        check_schema(&schema, "")?;
        Ok(Self { schema })
    }

    pub fn schema(&self) -> &Value {
        &self.schema
    }

    /// The OpenAI tool definition offered to the model on every round.
    pub fn tool_definition(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": FINISH_TOOL_NAME,
                "description": "End this run and hand back its result. Call it exactly once, on \
                                its own, when the task is complete. `result` must match the \
                                schema; a result that does not is returned to you with the \
                                reason, and the run continues.",
                "parameters": {
                    "type": "object",
                    "properties": { "result": self.schema },
                    "required": ["result"],
                },
            }
        })
    }

    /// The standing instruction folded into the run's leading system message.
    pub fn instructions(&self) -> &'static str {
        "This is a non-interactive run: nobody reads your messages while it runs. It ends only \
         when you call the `finish` tool with a `result` that matches its schema. Writing an \
         answer as text does not end it. Do the work with your tools, then call `finish`."
    }

    /// Check one `finish` call's raw arguments. `Ok` is the accepted result;
    /// `Err` is the message to answer the call with, so the model can fix it.
    pub fn check_call(&self, arguments_raw: &str) -> Result<Value, String> {
        let raw = if arguments_raw.trim().is_empty() {
            "{}"
        } else {
            arguments_raw
        };
        let args: Value = serde_json::from_str(raw).map_err(|e| {
            format!(
                "finish was not accepted: its arguments are not valid JSON ({e}). Call finish \
                 again with {{\"result\": …}} matching the schema."
            )
        })?;
        let Some(result) = args.get("result") else {
            return Err(
                "finish was not accepted: it needs a `result` argument. Call finish \
                        again with {\"result\": …} matching the schema."
                    .to_string(),
            );
        };
        let errors = validate(&self.schema, result);
        if errors.is_empty() {
            Ok(result.clone())
        } else {
            Err(format!(
                "finish was not accepted: the result does not match the required schema: {}. \
                 Fix these and call finish again.",
                errors.join("; ")
            ))
        }
    }

    /// Shape the last round the budget allows: only `finish` may still run,
    /// and the model is told to either call it or say what is left undone.
    ///
    /// `finish` replaces the whole tool list instead of riding along with
    /// `tool_choice: "none"`: `none` would forbid the one call that matters,
    /// and backends that ignore `tool_choice` (see
    /// `runner::configure_final_tool_round`) would run on with every tool. A
    /// single definition left in place also keeps the request well-formed for
    /// backends that reject tool history without any tools.
    pub fn prepare_final_round(&self, body: &mut Value) {
        const NOTICE: &str = "This is your FINAL round for this run: your tool budget is spent \
                              and only `finish` can still run. If the task is complete, call \
                              `finish` with the result now. If it is not, do not call `finish`: \
                              write a short account of what you did and what is left undone, \
                              and the run will be recorded as incomplete.";
        if let Some(obj) = body.as_object_mut() {
            obj.insert("tools".into(), json!([self.tool_definition()]));
            obj.remove("tool_choice");
        }
        if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
            crate::server::tools::runner::merge_into_leading_system_message(messages, NOTICE);
        }
    }

    /// Add the `finish` definition to a round's request.
    pub fn inject(&self, body: &mut Value) {
        let Some(obj) = body.as_object_mut() else {
            return;
        };
        let tools = obj.entry("tools").or_insert_with(|| json!([]));
        if let Some(list) = tools.as_array_mut() {
            list.retain(|t| {
                t.pointer("/function/name").and_then(Value::as_str) != Some(FINISH_TOOL_NAME)
            });
            list.push(self.tool_definition());
        }
    }
}

/// The nudge a contracted run gets after a round of text without `finish`.
pub const FINISH_NUDGE: &str = "You wrote a reply but did not call `finish`, so this run has not \
                                ended. If the task is complete, call `finish` with the result \
                                now. If it is not, continue working with your tools.";

/// The gateway's own account of a run that stopped without the model saying
/// what it did.
pub fn gateway_summary(rounds: u32, tools_run: &[String]) -> String {
    let mut names: Vec<&str> = tools_run.iter().map(String::as_str).collect();
    names.sort_unstable();
    names.dedup();
    let tools = if names.is_empty() {
        "no tools".to_string()
    } else {
        format!("tools: {}", names.join(", "))
    };
    format!(
        "The model gave no account of its work. It ran {rounds} round(s) ({tools}) without \
         calling finish with a valid result."
    )
}

/// The outcome slot one contracted run settles into. The driver writes it; the
/// caller that started the run takes it afterwards.
#[derive(Debug)]
pub struct FinishRun {
    contract: FinishContract,
    outcome: Mutex<Option<RunOutcome>>,
}

impl FinishRun {
    pub fn new(contract: FinishContract) -> Arc<Self> {
        Arc::new(Self {
            contract,
            outcome: Mutex::new(None),
        })
    }

    pub fn contract(&self) -> &FinishContract {
        &self.contract
    }

    /// Record how the run ended. The first outcome wins: once a run has
    /// finished, nothing that happens on its way out can overwrite that.
    pub fn settle(&self, outcome: RunOutcome) {
        let mut slot = self.outcome.lock().unwrap_or_else(|p| p.into_inner());
        if slot.is_none() {
            *slot = Some(outcome);
        }
    }

    pub fn take(&self) -> Option<RunOutcome> {
        self.outcome
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
    }
}

fn check_schema(schema: &Value, path: &str) -> Result<(), SchemaError> {
    let err = |problem: String| SchemaError {
        path: if path.is_empty() {
            "/".into()
        } else {
            path.into()
        },
        problem,
    };
    let Some(obj) = schema.as_object() else {
        return Err(err("a schema must be a JSON object".into()));
    };
    for key in obj.keys() {
        if !VALIDATING_KEYWORDS.contains(&key.as_str())
            && !ANNOTATION_KEYWORDS.contains(&key.as_str())
        {
            return Err(err(format!("the keyword `{key}` is not supported")));
        }
    }
    if let Some(ty) = obj.get("type") {
        let names: Vec<&Value> = match ty {
            Value::Array(list) => list.iter().collect(),
            other => vec![other],
        };
        for name in names {
            if !name.as_str().is_some_and(|n| TYPE_NAMES.contains(&n)) {
                return Err(err(format!("`type` names an unknown type {name}")));
            }
        }
    }
    if let Some(props) = obj.get("properties") {
        let Some(props) = props.as_object() else {
            return Err(err("`properties` must be an object".into()));
        };
        for (name, sub) in props {
            check_schema(sub, &format!("{path}/properties/{name}"))?;
        }
    }
    if let Some(required) = obj.get("required")
        && !required
            .as_array()
            .is_some_and(|r| r.iter().all(Value::is_string))
    {
        return Err(err("`required` must be a list of property names".into()));
    }
    if let Some(choices) = obj.get("enum")
        && !choices.is_array()
    {
        return Err(err("`enum` must be a list".into()));
    }
    if let Some(items) = obj.get("items") {
        check_schema(items, &format!("{path}/items"))?;
    }
    if let Some(extra) = obj.get("additionalProperties")
        && !extra.is_boolean()
    {
        return Err(err("`additionalProperties` must be true or false".into()));
    }
    Ok(())
}

/// Every way `value` breaks `schema`, each naming where. Empty when it fits.
pub fn validate(schema: &Value, value: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    validate_at(schema, value, "", &mut errors);
    errors
}

fn validate_at(schema: &Value, value: &Value, path: &str, errors: &mut Vec<String>) {
    let at = if path.is_empty() { "/" } else { path };
    if let Some(ty) = schema.get("type") {
        let allowed: Vec<&str> = match ty {
            Value::Array(list) => list.iter().filter_map(Value::as_str).collect(),
            other => other.as_str().into_iter().collect(),
        };
        if !allowed.iter().any(|t| has_type(value, t)) {
            errors.push(format!(
                "at `{at}`: expected {}, got {}",
                allowed.join(" or "),
                type_name(value)
            ));
            return;
        }
    }
    if let Some(choices) = schema.get("enum").and_then(Value::as_array)
        && !choices.contains(value)
    {
        let listed: Vec<String> = choices.iter().map(Value::to_string).collect();
        errors.push(format!(
            "at `{at}`: expected one of {}, got {value}",
            listed.join(", ")
        ));
    }
    if let Some(obj) = value.as_object() {
        let props = schema.get("properties").and_then(Value::as_object);
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for name in required.iter().filter_map(Value::as_str) {
                if !obj.contains_key(name) {
                    errors.push(format!("at `{at}`: missing required property `{name}`"));
                }
            }
        }
        for (name, sub_value) in obj {
            let sub_path = format!("{path}/{name}");
            match props.and_then(|p| p.get(name)) {
                Some(sub_schema) => validate_at(sub_schema, sub_value, &sub_path, errors),
                None if schema.get("additionalProperties") == Some(&Value::Bool(false)) => {
                    errors.push(format!("at `{sub_path}`: property `{name}` is not allowed"));
                }
                None => {}
            }
        }
    }
    if let (Some(items), Some(list)) = (schema.get("items"), value.as_array()) {
        for (i, item) in list.iter().enumerate() {
            validate_at(items, item, &format!("{path}/{i}"), errors);
        }
    }
}

fn has_type(value: &Value, ty: &str) -> bool {
    match ty {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => false,
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticket_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "status": {"type": "string", "enum": ["resolved", "escalated"]},
                "notes": {"type": "array", "items": {"type": "string"}},
                "count": {"type": "integer"},
            },
            "required": ["status"],
            "additionalProperties": false,
        })
    }

    fn contract() -> FinishContract {
        FinishContract::new(ticket_schema()).unwrap()
    }

    #[test]
    fn a_matching_value_has_no_errors() {
        let ok = json!({"status": "resolved", "notes": ["a", "b"], "count": 3});
        assert!(validate(&ticket_schema(), &ok).is_empty());
    }

    #[test]
    fn every_violation_is_reported_with_its_location() {
        let bad = json!({"notes": ["a", 7], "count": 1.5, "extra": true});
        let errors = validate(&ticket_schema(), &bad);
        let all = errors.join("\n");
        assert!(all.contains("missing required property `status`"), "{all}");
        assert!(
            all.contains("at `/notes/1`: expected string, got integer"),
            "{all}"
        );
        assert!(
            all.contains("at `/count`: expected integer, got number"),
            "{all}"
        );
        assert!(
            all.contains("at `/extra`: property `extra` is not allowed"),
            "{all}"
        );
        assert_eq!(errors.len(), 4, "{all}");
    }

    #[test]
    fn enum_and_type_lists_are_enforced() {
        let errors = validate(&ticket_schema(), &json!({"status": "done"}));
        assert_eq!(
            errors,
            vec![r#"at `/status`: expected one of "resolved", "escalated", got "done""#]
        );
        let nullable = json!({"type": ["string", "null"]});
        assert!(validate(&nullable, &Value::Null).is_empty());
        assert_eq!(
            validate(&nullable, &json!(1)),
            vec!["at `/`: expected string or null, got integer"]
        );
    }

    #[test]
    fn unknown_properties_pass_unless_forbidden() {
        let open = json!({"type": "object", "properties": {"a": {"type": "string"}}});
        assert!(validate(&open, &json!({"a": "x", "b": 1})).is_empty());
    }

    #[test]
    fn a_schema_with_an_unenforced_keyword_is_refused() {
        let err = FinishContract::new(json!({
            "type": "object",
            "properties": {"email": {"type": "string", "pattern": ".+@.+"}},
        }))
        .unwrap_err();
        assert_eq!(err.path, "/properties/email");
        assert!(err.problem.contains("`pattern`"), "{err}");
        assert!(err.to_string().contains("rewrite the schema"), "{err}");
    }

    #[test]
    fn a_malformed_schema_is_refused() {
        for (schema, needle) in [
            (json!("object"), "must be a JSON object"),
            (json!({"type": "thing"}), "unknown type"),
            (json!({"required": "status"}), "`required`"),
            (json!({"enum": "a"}), "`enum`"),
            (
                json!({"additionalProperties": {}}),
                "`additionalProperties`",
            ),
            (json!({"items": {"oneOf": []}}), "`oneOf`"),
        ] {
            let err = FinishContract::new(schema.clone()).unwrap_err();
            assert!(err.to_string().contains(needle), "{schema}: {err}");
        }
    }

    #[test]
    fn annotations_are_accepted() {
        FinishContract::new(json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "t", "description": "d", "default": {}, "examples": [],
            "type": "object",
        }))
        .unwrap();
    }

    #[test]
    fn a_valid_call_yields_its_result() {
        let result = contract()
            .check_call(r#"{"result": {"status": "escalated"}}"#)
            .unwrap();
        assert_eq!(result, json!({"status": "escalated"}));
    }

    #[test]
    fn an_invalid_call_explains_what_to_fix() {
        let msg = contract()
            .check_call(r#"{"result": {"status": "done"}}"#)
            .unwrap_err();
        assert!(msg.contains("does not match the required schema"), "{msg}");
        assert!(msg.contains("/status"), "{msg}");
        assert!(msg.contains("call finish again"), "{msg}");

        let msg = contract()
            .check_call(r#"{"status": "resolved"}"#)
            .unwrap_err();
        assert!(msg.contains("needs a `result` argument"), "{msg}");

        let msg = contract().check_call("{not json").unwrap_err();
        assert!(msg.contains("not valid JSON"), "{msg}");

        let msg = contract().check_call("").unwrap_err();
        assert!(msg.contains("needs a `result` argument"), "{msg}");
    }

    #[test]
    fn the_tool_definition_wraps_the_schema_as_result() {
        let def = contract().tool_definition();
        assert_eq!(def["function"]["name"], FINISH_TOOL_NAME);
        assert_eq!(def["function"]["parameters"]["required"], json!(["result"]));
        assert_eq!(
            def["function"]["parameters"]["properties"]["result"],
            ticket_schema()
        );
    }

    #[test]
    fn inject_adds_finish_once() {
        let mut body = json!({"tools": [{"type": "function", "function": {"name": "echo"}}]});
        contract().inject(&mut body);
        contract().inject(&mut body);
        let names: Vec<&str> = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["echo", FINISH_TOOL_NAME]);

        let mut bare = json!({"messages": []});
        contract().inject(&mut bare);
        assert_eq!(bare["tools"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn the_final_round_offers_only_finish_and_says_so() {
        let mut body = json!({
            "messages": [{"role": "system", "content": "rules"}, {"role": "user", "content": "go"}],
            "tools": [{"type": "function", "function": {"name": "echo"}}],
            "tool_choice": "auto",
        });
        contract().prepare_final_round(&mut body);
        assert_eq!(body["tools"], json!([contract().tool_definition()]));
        assert!(body.get("tool_choice").is_none());
        let system = body["messages"][0]["content"].as_str().unwrap();
        assert!(system.starts_with("rules"), "{system}");
        assert!(system.contains("FINAL round"), "{system}");
        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn the_first_settled_outcome_wins() {
        let run = FinishRun::new(contract());
        run.settle(RunOutcome::Finished {
            result: json!({"status": "resolved"}),
        });
        run.settle(RunOutcome::Incomplete {
            reason: IncompleteReason::Cancelled,
            summary: String::new(),
        });
        assert_eq!(
            run.take(),
            Some(RunOutcome::Finished {
                result: json!({"status": "resolved"})
            })
        );
        assert_eq!(run.take(), None);
    }

    #[test]
    fn outcomes_round_trip_through_json() {
        for outcome in [
            RunOutcome::Finished {
                result: json!({"status": "resolved"}),
            },
            RunOutcome::Incomplete {
                reason: IncompleteReason::RoundBudgetExhausted { rounds: 8 },
                summary: "did a, not b".into(),
            },
            RunOutcome::Incomplete {
                reason: IncompleteReason::Failed {
                    message: "upstream 500".into(),
                },
                summary: String::new(),
            },
            RunOutcome::Incomplete {
                reason: IncompleteReason::OutputTruncated,
                summary: String::new(),
            },
            RunOutcome::Incomplete {
                reason: IncompleteReason::RepeatedToolCall {
                    tool: "search".into(),
                },
                summary: String::new(),
            },
        ] {
            let wire = serde_json::to_value(&outcome).unwrap();
            assert_eq!(serde_json::from_value::<RunOutcome>(wire).unwrap(), outcome);
        }
        assert_eq!(
            serde_json::to_value(RunOutcome::Incomplete {
                reason: IncompleteReason::RoundBudgetExhausted { rounds: 8 },
                summary: "s".into(),
            })
            .unwrap(),
            json!({"status": "incomplete", "reason": {"kind": "round_budget_exhausted", "rounds": 8}, "summary": "s"})
        );
    }

    #[test]
    fn the_gateway_summary_names_the_tools_once() {
        let s = gateway_summary(3, &["search".into(), "fetch".into(), "search".into()]);
        assert!(s.contains("3 round(s)"), "{s}");
        assert!(s.contains("tools: fetch, search"), "{s}");
        assert!(gateway_summary(2, &[]).contains("no tools"));
    }
}
