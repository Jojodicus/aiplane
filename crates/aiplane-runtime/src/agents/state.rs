// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Typed conversation state (`docs/agents.md` §3 "State", issue #85).
//!
//! A spec's `state` declares slots, each with a type, constraints and the
//! writers allowed to set it (`set_by`). This module gives those declarations
//! run-time meaning:
//!
//! - [`StateSchema`] is the typed form of `state`, read from a spec the
//!   validator in [`super::spec`] accepted.
//! - [`SlotDef::check`] validates a value in code and says what to fix.
//! - Writes go through two doors. The model's door is the `set_<slot>` tool
//!   ([`super::slot_tools`]), which always writes [`Provenance::Llm`]. Every
//!   other provenance comes only through [`write_trusted`], which takes a
//!   [`TrustedWriter`] — a value Rust code constructs, never one parsed from
//!   a tool call. Both doors refuse a writer the slot's `set_by` does not
//!   list.
//! - [`AgentState`] is one conversation's slots as loaded, each `set`,
//!   `missing` or `invalid`; [`AgentState::view`] is what the model may see of
//!   it, which never includes a value the model did not write itself.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use aiplane_core::server::db::agent_state::{self, StoredSlot};
use aiplane_core::server::db::{DbError, Pool};
use jiff::Timestamp;
use regex::Regex;
use serde::Serialize;
use serde_json::{Value, json};

use super::slot_tools::set_tool_name;
use super::spec::SpecIssue;

/// Who wrote a slot's current value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Provenance {
    Llm,
    Verifier(String),
    Host,
}

impl Provenance {
    /// `llm`, `host` or `verifier:<id>` — the form stored and written in specs.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "llm" => Some(Self::Llm),
            "host" => Some(Self::Host),
            other => other
                .strip_prefix("verifier:")
                .filter(|id| !id.is_empty())
                .map(|id| Self::Verifier(id.to_string())),
        }
    }
}

impl fmt::Display for Provenance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Llm => f.write_str("llm"),
            Self::Host => f.write_str("host"),
            Self::Verifier(id) => write!(f, "verifier:{id}"),
        }
    }
}

impl Serialize for Provenance {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

/// A writer other than the model. There is deliberately no conversion from a
/// string or from JSON: a verifier or the host-JWT path names itself in code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustedWriter {
    Verifier(String),
    Host,
}

impl From<TrustedWriter> for Provenance {
    fn from(w: TrustedWriter) -> Self {
        match w {
            TrustedWriter::Verifier(id) => Provenance::Verifier(id),
            TrustedWriter::Host => Provenance::Host,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SlotType {
    String,
    Email,
    Enum(Vec<Value>),
    Integer,
    Number,
    Boolean,
    /// Who the conversation is about, as a verifier or the host vouched for
    /// it: an object, checked against `schema` when the spec gives one.
    Subject(Option<Value>),
}

/// One declared slot.
#[derive(Debug, Clone)]
pub struct SlotDef {
    pub name: String,
    pub ty: SlotType,
    pub set_by: Vec<Provenance>,
    pub description: Option<String>,
    min_length: Option<u64>,
    max_length: Option<u64>,
    minimum: Option<f64>,
    maximum: Option<f64>,
    pattern: Option<Regex>,
}

impl SlotDef {
    pub fn writable_by(&self, writer: &Provenance) -> bool {
        self.set_by.contains(writer)
    }

    /// Whether a `set_<slot>` tool exists for this slot.
    pub fn model_writable(&self) -> bool {
        self.writable_by(&Provenance::Llm)
    }

    /// `Ok` when `value` fits the slot; otherwise what is wrong and what a
    /// valid value looks like.
    pub fn check(&self, value: &Value) -> Result<(), String> {
        match &self.ty {
            SlotType::String => {
                let text = self.text(value)?;
                if let Some(re) = &self.pattern
                    && !re.is_match(text)
                {
                    return Err(format!(
                        "`{text}` does not match the required pattern `{}`",
                        re.as_str()
                    ));
                }
                Ok(())
            }
            SlotType::Email => {
                let text = self.text(value)?;
                if is_email(text) {
                    Ok(())
                } else {
                    Err(format!(
                        "`{text}` is not an email address — write it as name@example.com"
                    ))
                }
            }
            SlotType::Enum(values) => {
                if values.contains(value) {
                    Ok(())
                } else {
                    let listed: Vec<String> = values.iter().map(Value::to_string).collect();
                    Err(format!("must be one of: {}", listed.join(", ")))
                }
            }
            SlotType::Integer => {
                if value.as_i64().is_none() && value.as_u64().is_none() {
                    return Err(format!("must be a whole number, not {}", kind(value)));
                }
                self.range(value)
            }
            SlotType::Number => {
                if !value.is_number() {
                    return Err(format!("must be a number, not {}", kind(value)));
                }
                self.range(value)
            }
            SlotType::Boolean => {
                if value.is_boolean() {
                    Ok(())
                } else {
                    Err(format!("must be true or false, not {}", kind(value)))
                }
            }
            SlotType::Subject(schema) => {
                if !value.is_object() {
                    return Err(format!("must be an object, not {}", kind(value)));
                }
                let errors = schema
                    .as_ref()
                    .map(|s| crate::finish::validate(s, value))
                    .unwrap_or_default();
                if errors.is_empty() {
                    Ok(())
                } else {
                    Err(errors.join("; "))
                }
            }
        }
    }

    /// A string slot's text, within its length bounds.
    fn text<'v>(&self, value: &'v Value) -> Result<&'v str, String> {
        let Some(text) = value.as_str() else {
            return Err(format!("must be text, not {}", kind(value)));
        };
        let len = text.chars().count() as u64;
        if let Some(min) = self.min_length
            && len < min
        {
            return Err(format!("must be at least {min} characters long, not {len}"));
        }
        if let Some(max) = self.max_length
            && len > max
        {
            return Err(format!(
                "must be at most {max} characters long, not {len} — shorten it"
            ));
        }
        Ok(text)
    }

    fn range(&self, value: &Value) -> Result<(), String> {
        let n = value.as_f64().unwrap_or_default();
        if let Some(min) = self.minimum
            && n < min
        {
            return Err(format!("must be at least {}, not {value}", number(min)));
        }
        if let Some(max) = self.maximum
            && n > max
        {
            return Err(format!("must be at most {}, not {value}", number(max)));
        }
        Ok(())
    }

    /// The JSON Schema of the value, as the `set_<slot>` tool advertises it.
    /// Informational for the model; [`Self::check`] is what is enforced.
    pub fn value_schema(&self) -> Value {
        let mut schema = match &self.ty {
            SlotType::String => json!({ "type": "string" }),
            SlotType::Email => json!({ "type": "string", "format": "email" }),
            SlotType::Enum(values) => json!({ "enum": values }),
            SlotType::Integer => json!({ "type": "integer" }),
            SlotType::Number => json!({ "type": "number" }),
            SlotType::Boolean => json!({ "type": "boolean" }),
            SlotType::Subject(schema) => schema.clone().unwrap_or(json!({ "type": "object" })),
        };
        if let Some(obj) = schema.as_object_mut() {
            let mut put = |key: &str, v: Option<Value>| {
                if let Some(v) = v {
                    obj.insert(key.to_string(), v);
                }
            };
            put("minLength", self.min_length.map(Value::from));
            put("maxLength", self.max_length.map(Value::from));
            put("minimum", self.minimum.map(Value::from));
            put("maximum", self.maximum.map(Value::from));
            put(
                "pattern",
                self.pattern.as_ref().map(|re| Value::from(re.as_str())),
            );
            put("description", self.description.clone().map(Value::from));
        }
        schema
    }

    fn from_spec(name: &str, def: &Value) -> Result<Self, String> {
        let Value::Object(map) = def else {
            return Err("a slot must be an object".into());
        };
        let ty = match map.get("type").and_then(Value::as_str) {
            Some("string") => SlotType::String,
            Some("email") => SlotType::Email,
            Some("enum") => match map.get("values") {
                Some(Value::Array(values)) if !values.is_empty() => SlotType::Enum(values.clone()),
                _ => return Err("an `enum` slot needs a non-empty `values` list".into()),
            },
            Some("integer") => SlotType::Integer,
            Some("number") => SlotType::Number,
            Some("boolean") => SlotType::Boolean,
            Some("subject") => SlotType::Subject(map.get("schema").cloned()),
            Some(other) => return Err(format!("`{other}` is not a slot type")),
            None => return Err("a slot needs a `type`".into()),
        };
        let set_by = match map.get("set_by") {
            Some(Value::Array(writers)) => writers
                .iter()
                .map(|w| {
                    w.as_str()
                        .and_then(Provenance::parse)
                        .ok_or_else(|| format!("`{w}` in `set_by` is not a writer"))
                })
                .collect::<Result<Vec<_>, _>>()?,
            _ => return Err("a slot needs a `set_by` list".into()),
        };
        let pattern = match map.get("pattern") {
            Some(p) => Some(
                p.as_str()
                    .and_then(|p| Regex::new(p).ok())
                    .ok_or("`pattern` is not a valid regular expression")?,
            ),
            None => None,
        };
        Ok(Self {
            name: name.to_string(),
            ty,
            set_by,
            description: map
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string),
            min_length: map.get("min_length").and_then(Value::as_u64),
            max_length: map.get("max_length").and_then(Value::as_u64),
            minimum: map.get("minimum").and_then(Value::as_f64),
            maximum: map.get("maximum").and_then(Value::as_f64),
            pattern,
        })
    }
}

/// The typed form of a spec's `state`.
#[derive(Debug, Clone, Default)]
pub struct StateSchema {
    slots: BTreeMap<String, SlotDef>,
}

impl StateSchema {
    /// Read `state` from a spec. A spec the validator accepted always reads;
    /// the error exists for one that skipped it, and names the first problem.
    pub fn from_spec(spec: &Value) -> Result<Self, SpecIssue> {
        let Some(state) = spec.get("state") else {
            return Ok(Self::default());
        };
        let Value::Object(map) = state else {
            return Err(SpecIssue {
                path: "state".into(),
                message: "must be an object keyed by slot name".into(),
            });
        };
        let mut slots = BTreeMap::new();
        for (name, def) in map {
            let def = SlotDef::from_spec(name, def).map_err(|message| SpecIssue {
                path: format!("state.{name}"),
                message,
            })?;
            slots.insert(name.clone(), def);
        }
        Ok(Self { slots })
    }

    pub fn slot(&self, name: &str) -> Option<&SlotDef> {
        self.slots.get(name)
    }

    /// In name order.
    pub fn slots(&self) -> impl Iterator<Item = &SlotDef> {
        self.slots.values()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

/// A slot's stored value and where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotEntry {
    pub value: Value,
    pub provenance: Provenance,
    pub set_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SlotState {
    Missing,
    Set(SlotEntry),
    /// Stored, but no longer acceptable — the spec changed since it was
    /// written, so the type or the writer no longer fits. Treated as missing
    /// by every gate.
    Invalid {
        entry: SlotEntry,
        reason: String,
    },
}

/// One conversation's slots, read once and judged against the schema.
#[derive(Debug, Clone, Default)]
pub struct AgentState {
    slots: BTreeMap<String, SlotState>,
}

const MISSING: SlotState = SlotState::Missing;

impl AgentState {
    pub async fn load(
        pool: &Pool,
        schema: &StateSchema,
        session_id: &str,
    ) -> Result<Self, DbError> {
        let rows = agent_state::for_session(pool, session_id).await?;
        Ok(Self::from_rows(schema, rows))
    }

    /// Judge stored rows against `schema`. Rows for slots the schema no longer
    /// declares are dropped.
    pub fn from_rows(schema: &StateSchema, rows: Vec<StoredSlot>) -> Self {
        let mut slots = BTreeMap::new();
        for row in rows {
            let Some(def) = schema.slot(&row.slot) else {
                continue;
            };
            let Some(provenance) = Provenance::parse(&row.provenance) else {
                continue;
            };
            let entry = SlotEntry {
                value: row.value,
                provenance,
                set_at: row.set_at,
            };
            let judged = if !def.writable_by(&entry.provenance) {
                SlotState::Invalid {
                    reason: format!(
                        "was written by `{}`, which may no longer set this slot",
                        entry.provenance
                    ),
                    entry,
                }
            } else {
                match def.check(&entry.value) {
                    Ok(()) => SlotState::Set(entry),
                    // The model may hear what is wrong with its own value; a
                    // trusted value is not echoed back, not even in an error.
                    Err(reason) if entry.provenance == Provenance::Llm => {
                        SlotState::Invalid { entry, reason }
                    }
                    Err(_) => SlotState::Invalid {
                        entry,
                        reason: "the stored value no longer fits the slot's type".into(),
                    },
                }
            };
            slots.insert(row.slot, judged);
        }
        Self { slots }
    }

    /// [`SlotState::Missing`] for a slot that is not declared at all.
    pub fn get(&self, slot: &str) -> &SlotState {
        self.slots.get(slot).unwrap_or(&MISSING)
    }

    /// The entry, only when it is [`SlotState::Set`].
    pub fn valid(&self, slot: &str) -> Option<&SlotEntry> {
        match self.get(slot) {
            SlotState::Set(entry) => Some(entry),
            _ => None,
        }
    }

    /// What the model may see, in slot order.
    pub fn view(&self, schema: &StateSchema) -> Vec<SlotView> {
        schema
            .slots()
            .map(|def| {
                let (status, reason, value, by) = match self.get(&def.name) {
                    SlotState::Missing => (SlotStatus::Missing, None, None, None),
                    SlotState::Invalid { reason, .. } => {
                        (SlotStatus::Invalid, Some(reason.clone()), None, None)
                    }
                    SlotState::Set(entry) => (
                        SlotStatus::Set,
                        None,
                        (entry.provenance == Provenance::Llm).then(|| entry.value.clone()),
                        Some(entry.provenance.clone()),
                    ),
                };
                SlotView {
                    slot: def.name.clone(),
                    status,
                    reason,
                    value,
                    by,
                    set_by: def.set_by.clone(),
                    tool: def.model_writable().then(|| set_tool_name(&def.name)),
                }
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotStatus {
    Set,
    Missing,
    Invalid,
}

/// One slot as the model sees it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SlotView {
    pub slot: String,
    pub status: SlotStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Only for a value the model wrote itself; a verifier's or the host's
    /// value never reaches the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// Who wrote the current value, when it is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<Provenance>,
    pub set_by: Vec<Provenance>,
    /// The `set_<slot>` tool to call, when the model may write the slot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
}

/// The slot block of the agent's system message.
pub fn render_view(views: &[SlotView]) -> String {
    if views.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "Conversation state. A value you did not write yourself is never shown to you.",
    );
    for v in views {
        let line = match (&v.status, &v.value, &v.by) {
            (SlotStatus::Set, Some(value), _) => format!("{}: set to {value}", v.slot),
            (SlotStatus::Set, None, by) => format!(
                "{}: set by {}",
                v.slot,
                by.as_ref().map(ToString::to_string).unwrap_or_default()
            ),
            (SlotStatus::Missing, ..) => format!("{}: missing{}", v.slot, how_to_set(v)),
            (SlotStatus::Invalid, ..) => format!(
                "{}: invalid: {}{}",
                v.slot,
                v.reason.as_deref().unwrap_or_default(),
                how_to_set(v)
            ),
        };
        out.push_str("\n- ");
        out.push_str(&line);
    }
    out
}

fn how_to_set(v: &SlotView) -> String {
    match &v.tool {
        Some(tool) => format!(" — call {tool}"),
        None => format!(" — set by {}, not by you", writers(&v.set_by)),
    }
}

fn writers(set_by: &[Provenance]) -> String {
    let names: Vec<String> = set_by.iter().map(ToString::to_string).collect();
    names.join(" or ")
}

/// `local@domain.tld`, without whitespace. Deliverability is the verifier's
/// business; this only refuses what cannot be an address.
fn is_email(s: &str) -> bool {
    if s.len() > 254 || s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    let labels: Vec<&str> = domain.split('.').collect();
    !local.is_empty()
        && local.len() <= 64
        && labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty()
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars().all(|c| c.is_alphanumeric() || c == '-')
        })
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "text",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// `1`, not `1.0`, for a bound written as a whole number.
fn number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        n.to_string()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StateWriteError {
    #[error("there is no slot `{slot}` in this agent's state; the declared slots are: {known}")]
    UnknownSlot { slot: String, known: String },
    #[error(
        "slot `{slot}` cannot be written by `{writer}`: only {allowed} may set it. A value from \
         anyone else is never stored"
    )]
    NotWritable {
        slot: String,
        writer: Provenance,
        allowed: String,
    },
    #[error("slot `{slot}` was not set: {message}")]
    Invalid { slot: String, message: String },
    #[error("storing slot `{slot}`: {source}")]
    Db {
        slot: String,
        #[source]
        source: DbError,
    },
}

/// The clock writes are stamped with. Injected so tests can fix `set_at`, and
/// with it every `max_age` gate built on top.
pub type Clock = Arc<dyn Fn() -> Timestamp + Send + Sync>;

pub fn system_clock() -> Clock {
    Arc::new(Timestamp::now)
}

/// The door for verifiers and the host. The only way to store a provenance
/// other than `llm`.
pub async fn write_trusted(
    pool: &Pool,
    schema: &StateSchema,
    session_id: &str,
    slot: &str,
    value: Value,
    writer: TrustedWriter,
    now: Timestamp,
) -> Result<SlotEntry, StateWriteError> {
    write(pool, schema, session_id, slot, value, writer.into(), now).await
}

/// The model's door; only the `set_<slot>` tool calls it.
pub(crate) async fn write_from_model(
    pool: &Pool,
    schema: &StateSchema,
    session_id: &str,
    slot: &str,
    value: Value,
    now: Timestamp,
) -> Result<SlotEntry, StateWriteError> {
    write(pool, schema, session_id, slot, value, Provenance::Llm, now).await
}

async fn write(
    pool: &Pool,
    schema: &StateSchema,
    session_id: &str,
    slot: &str,
    value: Value,
    provenance: Provenance,
    now: Timestamp,
) -> Result<SlotEntry, StateWriteError> {
    let Some(def) = schema.slot(slot) else {
        let known: Vec<&str> = schema.slots().map(|d| d.name.as_str()).collect();
        return Err(StateWriteError::UnknownSlot {
            slot: slot.to_string(),
            known: known.join(", "),
        });
    };
    // The writer is checked before the value, so a refused writer learns
    // nothing about what the slot would have accepted.
    if !def.writable_by(&provenance) {
        return Err(StateWriteError::NotWritable {
            slot: slot.to_string(),
            writer: provenance,
            allowed: writers(&def.set_by),
        });
    }
    def.check(&value)
        .map_err(|message| StateWriteError::Invalid {
            slot: slot.to_string(),
            message,
        })?;
    agent_state::put(pool, session_id, slot, &value, &provenance.to_string(), now)
        .await
        .map_err(|source| StateWriteError::Db {
            slot: slot.to_string(),
            source,
        })?;
    Ok(SlotEntry {
        value,
        provenance,
        set_at: now,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::Path;

    pub(crate) fn support_spec() -> Value {
        json!({
            "verifiers": { "otp": { "kind": "mcp_code" } },
            "state": {
                "name": { "type": "string", "min_length": 2, "max_length": 10, "set_by": ["llm"],
                          "description": "the visitor's name" },
                "email": { "type": "email", "set_by": ["llm"] },
                "issue": { "type": "enum", "values": ["billing", "technical"], "set_by": ["llm"] },
                "order": { "type": "string", "pattern": "^RE-\\d{6}$", "set_by": ["llm"] },
                "seats": { "type": "integer", "minimum": 1, "maximum": 50, "set_by": ["llm"] },
                "score": { "type": "number", "maximum": 1, "set_by": ["llm", "host"] },
                "urgent": { "type": "boolean", "set_by": ["llm"] },
                "verified": { "type": "subject", "set_by": ["verifier:otp", "host"],
                              "schema": { "type": "object", "required": ["customer_id"],
                                          "properties": { "customer_id": { "type": "string" } } } },
                "plan": { "type": "string", "set_by": ["host"] }
            }
        })
    }

    pub(crate) fn schema() -> StateSchema {
        StateSchema::from_spec(&support_spec()).unwrap()
    }

    pub(crate) fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    pub(crate) async fn pool_with_session(id: &str) -> Pool {
        let pool = aiplane_core::server::db::open(Path::new(":memory:"))
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO users (id, email, created_at, updated_at)
             VALUES ('u1', 'u1@example.com', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO chat_sessions (id, user_id, created_at, updated_at)
             VALUES (?, 'u1', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    fn slot(name: &str) -> SlotDef {
        schema().slot(name).unwrap().clone()
    }

    #[test]
    fn provenance_reads_and_writes_the_three_documented_forms() {
        for (raw, parsed) in [
            ("llm", Provenance::Llm),
            ("host", Provenance::Host),
            ("verifier:otp", Provenance::Verifier("otp".into())),
        ] {
            assert_eq!(Provenance::parse(raw), Some(parsed.clone()));
            assert_eq!(parsed.to_string(), raw);
        }
        for bad in ["", "LLM", "verifier:", "verifier", "model"] {
            assert_eq!(Provenance::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_schema_reads_every_slot_with_its_type_and_writers() {
        let s = schema();
        let names: Vec<&str> = s.slots().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "email", "issue", "name", "order", "plan", "score", "seats", "urgent", "verified"
            ]
        );
        assert_eq!(
            s.slot("issue").unwrap().ty,
            SlotType::Enum(vec![json!("billing"), json!("technical")])
        );
        let verified = s.slot("verified").unwrap();
        assert_eq!(
            verified.set_by,
            [Provenance::Verifier("otp".into()), Provenance::Host]
        );
        assert!(!verified.model_writable());
        assert!(s.slot("name").unwrap().model_writable());
        assert_eq!(
            s.slot("name").unwrap().description.as_deref(),
            Some("the visitor's name")
        );
    }

    #[test]
    fn a_spec_without_state_has_no_slots_and_a_malformed_one_names_the_problem() {
        assert_eq!(
            StateSchema::from_spec(&json!({})).unwrap().slots().count(),
            0
        );
        let err =
            StateSchema::from_spec(&json!({ "state": { "x": { "type": "date" } } })).unwrap_err();
        assert_eq!(err.path, "state.x");
    }

    /// Table: slot, value, `None` when accepted or a fragment of the reason.
    #[test]
    fn values_are_checked_against_the_slot_type_and_constraints() {
        let cases: &[(&str, Value, Option<&str>)] = &[
            ("name", json!("Ada"), None),
            ("name", json!("A"), Some("at least 2 characters")),
            ("name", json!("Bartholomew!"), Some("at most 10 characters")),
            ("name", json!("Zoë"), None),
            ("name", json!(7), Some("must be text")),
            ("email", json!("ada@example.com"), None),
            (
                "email",
                json!("ada@localhost"),
                Some("not an email address"),
            ),
            (
                "email",
                json!("ada example.com"),
                Some("not an email address"),
            ),
            ("email", json!("@example.com"), Some("not an email address")),
            (
                "email",
                json!("a@@example.com"),
                Some("not an email address"),
            ),
            (
                "email",
                json!("ada@exa mple.com"),
                Some("not an email address"),
            ),
            ("issue", json!("billing"), None),
            (
                "issue",
                json!("sales"),
                Some("one of: \"billing\", \"technical\""),
            ),
            ("order", json!("RE-123456"), None),
            ("order", json!("RE-12345"), Some("does not match")),
            ("seats", json!(3), None),
            ("seats", json!(0), Some("at least 1")),
            ("seats", json!(51), Some("at most 50")),
            ("seats", json!(2.5), Some("whole number")),
            ("seats", json!("3"), Some("whole number")),
            ("score", json!(0.5), None),
            ("score", json!(1.5), Some("at most 1")),
            ("score", json!("high"), Some("must be a number")),
            ("urgent", json!(true), None),
            ("urgent", json!("yes"), Some("true or false")),
            ("verified", json!({"customer_id": "K-1"}), None),
            (
                "verified",
                json!({"id": "K-1"}),
                Some("missing required property `customer_id`"),
            ),
            ("verified", json!("K-1"), Some("must be an object")),
        ];
        for (name, value, expected) in cases {
            let got = slot(name).check(value);
            match expected {
                None => assert_eq!(got, Ok(()), "{name} = {value}"),
                Some(fragment) => {
                    let msg = got.expect_err(&format!("{name} = {value} was accepted"));
                    assert!(msg.contains(fragment), "{name} = {value}: {msg}");
                }
            }
        }
    }

    #[test]
    fn the_advertised_value_schema_matches_the_slot() {
        assert_eq!(
            slot("name").value_schema(),
            json!({ "type": "string", "minLength": 2, "maxLength": 10,
                    "description": "the visitor's name" })
        );
        assert_eq!(
            slot("email").value_schema(),
            json!({ "type": "string", "format": "email" })
        );
        assert_eq!(
            slot("issue").value_schema(),
            json!({ "enum": ["billing", "technical"] })
        );
        assert_eq!(
            slot("seats").value_schema(),
            json!({ "type": "integer", "minimum": 1.0, "maximum": 50.0 })
        );
        assert_eq!(
            slot("order").value_schema(),
            json!({ "type": "string", "pattern": "^RE-\\d{6}$" })
        );
    }

    fn row(slot: &str, value: Value, provenance: &str) -> StoredSlot {
        StoredSlot {
            slot: slot.into(),
            value,
            provenance: provenance.into(),
            set_at: at("2026-10-02T10:00:00Z"),
        }
    }

    #[test]
    fn stored_rows_are_judged_set_missing_or_invalid() {
        let state = AgentState::from_rows(
            &schema(),
            vec![
                row("name", json!("Ada"), "llm"),
                row("issue", json!("sales"), "llm"),
                row("plan", json!("gold"), "llm"),
                row("verified", json!({"customer_id": "K-1"}), "verifier:otp"),
                row("retired", json!(1), "llm"),
            ],
        );
        assert!(matches!(state.get("name"), SlotState::Set(_)));
        assert!(matches!(state.get("email"), SlotState::Missing));
        assert!(matches!(state.get("retired"), SlotState::Missing));
        let SlotState::Invalid { reason, .. } = state.get("issue") else {
            panic!("{:?}", state.get("issue"));
        };
        assert!(reason.contains("one of"), "{reason}");
        let SlotState::Invalid { reason, .. } = state.get("plan") else {
            panic!("{:?}", state.get("plan"));
        };
        assert!(reason.contains("`llm`"), "{reason}");
        assert_eq!(
            state.valid("verified").map(|e| &e.provenance),
            Some(&Provenance::Verifier("otp".into()))
        );
        assert_eq!(state.valid("issue"), None);
    }

    #[test]
    fn the_view_shows_the_models_own_values_but_never_a_verifiers_or_the_hosts() {
        let state = AgentState::from_rows(
            &schema(),
            vec![
                row("name", json!("Ada"), "llm"),
                row("score", json!(0.9), "host"),
                row("verified", json!({"customer_id": "K-1"}), "verifier:otp"),
                row("issue", json!("sales"), "llm"),
                row("plan", json!(["not a string"]), "host"),
            ],
        );
        let view = state.view(&schema());
        let by: BTreeMap<&str, &SlotView> = view.iter().map(|v| (v.slot.as_str(), v)).collect();

        assert_eq!(by["name"].status, SlotStatus::Set);
        assert_eq!(by["name"].value, Some(json!("Ada")));
        assert_eq!(by["name"].tool.as_deref(), Some("set_name"));

        assert_eq!(by["verified"].status, SlotStatus::Set);
        assert_eq!(by["verified"].value, None);
        assert_eq!(by["verified"].tool, None);
        assert_eq!(by["score"].value, None);

        assert_eq!(by["email"].status, SlotStatus::Missing);
        assert_eq!(by["issue"].status, SlotStatus::Invalid);
        assert!(by["issue"].reason.as_deref().unwrap().contains("one of"));

        // A trusted value that went stale is reported without echoing it.
        assert_eq!(by["plan"].status, SlotStatus::Invalid);
        let reason = by["plan"].reason.as_deref().unwrap();
        assert!(!reason.contains("not a string"), "{reason}");

        let rendered = render_view(&view);
        assert!(!rendered.contains("K-1"), "{rendered}");
        assert!(!rendered.contains("0.9"), "{rendered}");
        assert!(rendered.contains("name: set to \"Ada\""), "{rendered}");
        assert!(
            rendered.contains("email: missing — call set_email"),
            "{rendered}"
        );
        assert!(
            rendered.contains("verified: set by verifier:otp"),
            "{rendered}"
        );
        let empty = AgentState::default().view(&schema());
        let rendered = render_view(&empty);
        assert!(
            rendered.contains("verified: missing — set by verifier:otp or host, not by you"),
            "{rendered}"
        );
    }

    #[tokio::test]
    async fn a_trusted_writer_stores_its_provenance_and_the_injected_time() {
        let pool = pool_with_session("s1").await;
        let s = schema();
        let now = at("2026-10-02T12:00:00Z");
        let entry = write_trusted(
            &pool,
            &s,
            "s1",
            "verified",
            json!({"customer_id": "K-7"}),
            TrustedWriter::Verifier("otp".into()),
            now,
        )
        .await
        .unwrap();
        assert_eq!(entry.provenance, Provenance::Verifier("otp".into()));

        let state = AgentState::load(&pool, &s, "s1").await.unwrap();
        assert_eq!(
            state.valid("verified"),
            Some(&SlotEntry {
                value: json!({"customer_id": "K-7"}),
                provenance: Provenance::Verifier("otp".into()),
                set_at: now,
            })
        );
    }

    #[tokio::test]
    async fn a_writer_outside_set_by_is_refused_and_nothing_is_stored() {
        let pool = pool_with_session("s1").await;
        let s = schema();
        let now = at("2026-10-02T12:00:00Z");

        let err = write_trusted(
            &pool,
            &s,
            "s1",
            "verified",
            json!({"customer_id": "K-7"}),
            TrustedWriter::Verifier("sms".into()),
            now,
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("only verifier:otp or host may set it"),
            "{err}"
        );

        let err = write_from_model(&pool, &s, "s1", "plan", json!("gold"), now)
            .await
            .unwrap_err();
        assert!(matches!(err, StateWriteError::NotWritable { .. }), "{err}");

        let err = write_trusted(
            &pool,
            &s,
            "s1",
            "name",
            json!("Ada"),
            TrustedWriter::Host,
            now,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("only llm may set it"), "{err}");

        assert!(
            agent_state::for_session(&pool, "s1")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn an_invalid_value_or_unknown_slot_is_refused_with_the_reason() {
        let pool = pool_with_session("s1").await;
        let s = schema();
        let now = at("2026-10-02T12:00:00Z");
        let err = write_from_model(&pool, &s, "s1", "issue", json!("sales"), now)
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("one of: \"billing\", \"technical\""),
            "{err}"
        );

        let err = write_trusted(&pool, &s, "s1", "ghost", json!(1), TrustedWriter::Host, now)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no slot `ghost`"), "{err}");
        assert!(err.to_string().contains("email, issue, name"), "{err}");
    }

    #[tokio::test]
    async fn a_model_rewrite_of_a_slot_a_trusted_writer_also_owns_takes_llm_provenance() {
        let pool = pool_with_session("s1").await;
        let s = schema();
        write_trusted(
            &pool,
            &s,
            "s1",
            "score",
            json!(0.2),
            TrustedWriter::Host,
            at("2026-10-02T12:00:00Z"),
        )
        .await
        .unwrap();
        write_from_model(
            &pool,
            &s,
            "s1",
            "score",
            json!(0.9),
            at("2026-10-02T12:05:00Z"),
        )
        .await
        .unwrap();
        let state = AgentState::load(&pool, &s, "s1").await.unwrap();
        let entry = state.valid("score").unwrap();
        assert_eq!(entry.provenance, Provenance::Llm);
        assert_eq!(entry.set_at, at("2026-10-02T12:05:00Z"));
    }
}
