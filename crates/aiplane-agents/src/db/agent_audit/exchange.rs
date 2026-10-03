// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! How the log keeps a model exchange's request small, and gives the whole
//! request back (`docs/agents.md` → "What #111 built", "Storage").
//!
//! **Deltas**, because a round's request repeats the whole conversation so
//! far. The stored format, which every signed event already holds and so
//! cannot change: the first round of a turn keeps `request`; every later one
//! `request_delta` against the round before — `prev` (that event's id),
//! `keep` (how many of its messages this request starts with), the
//! `messages` after them, the `system` message when it changed, `tools` when
//! the offer changed (`null` when it was dropped), and `rest`, every other
//! key.
//!
//! **Blobs**, because a base64 image is re-sent every round: a `data:` URL of
//! at least [`BLOB_MIN_BYTES`] is stored once per chain in `activity_blobs`,
//! by its SHA-256, and the detail holds [`BLOB_REF`]`<hash>`. The reference
//! is inside the event's signed hash, so a blob whose content no longer
//! matches it is not served.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use serde_json::{Map, Value};

use super::super::{DbError, Pool};
use super::StoredEvent;
use aiplane_core::server::crypto::sha256_hex;

/// The smallest `data:` URL stored as a blob. Below it, the reference and
/// its row cost about as much as the part.
pub const BLOB_MIN_BYTES: usize = 4096;

/// What stands in an exchange's detail for a part stored as a blob.
pub const BLOB_REF: &str = "activity-blob:sha256:";

/// How many whole requests a [`Reconstructor`] keeps for the deltas after
/// them.
const CACHED_REQUESTS: usize = 64;

/// `cur` as a delta against `prev`, the request of the round before; `None`
/// when they share no message beyond the system message, so `cur` is
/// better stored whole. The delta has no `prev` yet: the caller adds the
/// id of `prev`'s event.
pub fn request_delta(prev: &Value, cur: &Value) -> Option<Value> {
    let prev_messages = prev.get("messages")?.as_array()?;
    let messages = cur.get("messages")?.as_array()?;
    let system = |m: &[Value]| m.first().is_some_and(|m| m["role"] == "system");
    let from = usize::from(system(prev_messages) && system(messages));
    let keep = from
        + prev_messages[from..]
            .iter()
            .zip(&messages[from..])
            .take_while(|(a, b)| a == b)
            .count();
    if keep <= from {
        return None;
    }
    let mut delta = Map::new();
    delta.insert("keep".into(), keep.into());
    delta.insert("messages".into(), Value::Array(messages[keep..].to_vec()));
    if from == 1 && prev_messages[0] != messages[0] {
        delta.insert("system".into(), messages[0].clone());
    }
    if prev.get("tools") != cur.get("tools") {
        delta.insert(
            "tools".into(),
            cur.get("tools").cloned().unwrap_or(Value::Null),
        );
    }
    let rest: Map<String, Value> = cur
        .as_object()?
        .iter()
        .filter(|(k, _)| *k != "messages" && *k != "tools")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    delta.insert("rest".into(), Value::Object(rest));
    Some(Value::Object(delta))
}

/// The request `delta` was made from, given `prev`, the request it was made
/// against.
pub fn apply_delta(prev: &Value, delta: &Value) -> Value {
    let keep = delta["keep"].as_u64().unwrap_or(0) as usize;
    let mut messages: Vec<Value> = prev["messages"]
        .as_array()
        .map(|m| m.iter().take(keep).cloned().collect())
        .unwrap_or_default();
    if let (Some(system), Some(first)) = (delta.get("system"), messages.first_mut()) {
        *first = system.clone();
    }
    messages.extend(delta["messages"].as_array().into_iter().flatten().cloned());
    let mut out = delta["rest"].as_object().cloned().unwrap_or_default();
    out.insert("messages".into(), Value::Array(messages));
    match delta.get("tools").or_else(|| prev.get("tools")) {
        None | Some(Value::Null) => {}
        Some(tools) => {
            out.insert("tools".into(), tools.clone());
        }
    }
    Value::Object(out)
}

/// Every `data:` URL of at least [`BLOB_MIN_BYTES`] in `detail`, replaced
/// by its reference; returns `(hash, data)` for each.
pub fn take_blobs(detail: &mut Value) -> Vec<(String, String)> {
    let mut blobs = Vec::new();
    walk_strings(detail, &mut |s| {
        if s.len() < BLOB_MIN_BYTES || !s.starts_with("data:") || !s.contains(";base64,") {
            return;
        }
        let hash = sha256_hex(s.as_bytes());
        let data = std::mem::replace(s, format!("{BLOB_REF}{hash}"));
        blobs.push((hash, data));
    });
    blobs
}

fn walk_strings(v: &mut Value, f: &mut impl FnMut(&mut String)) {
    match v {
        Value::String(s) => f(s),
        Value::Array(items) => items.iter_mut().for_each(|i| walk_strings(i, f)),
        Value::Object(map) => map.values_mut().for_each(|i| walk_strings(i, f)),
        _ => {}
    }
}

/// Gives back what a stored model exchange stood for: its whole request,
/// rebuilt from the deltas before it, with its blobs put back. Keeps the
/// [`CACHED_REQUESTS`] newest requests it met or rebuilt, shared rather
/// than copied, so reading a turn in order rebuilds each request once.
#[derive(Default)]
pub struct Reconstructor {
    requests: HashMap<String, Arc<Value>>,
    /// The ids in `requests`, oldest first.
    order: VecDeque<String>,
}

impl Reconstructor {
    /// `event` as the activity API serves it ([`StoredEvent::to_json`]),
    /// with an exchange's `request_delta` replaced by the whole `request`
    /// and its blobs resolved.
    pub async fn event_json(&mut self, pool: &Pool, event: &StoredEvent) -> Result<Value, DbError> {
        let mut json = event.to_json();
        if event.kind != "llm_exchange" {
            return Ok(json);
        }
        let detail = &mut json["detail"];
        if let Some(request) = detail.get("request") {
            self.remember(event.id.clone(), Arc::new(request.clone()));
        } else if let Some(delta) = detail.get("request_delta")
            && let Some(request) = self.rebuild(pool, &event.id, delta).await?
            && let Some(map) = detail.as_object_mut()
        {
            map.remove("request_delta");
            map.insert("request".into(), Value::clone(&request));
        }
        if let Some(chain) = &event.chain_key {
            resolve_blobs(pool, chain, detail).await?;
        }
        Ok(json)
    }

    /// The whole request of exchange `id`, stored as `delta`, blobs still as
    /// references; `None` when the chain of `prev` behind it is broken.
    async fn rebuild(
        &mut self,
        pool: &Pool,
        id: &str,
        delta: &Value,
    ) -> Result<Option<Arc<Value>>, DbError> {
        if let Some(request) = self.requests.get(id) {
            return Ok(Some(request.clone()));
        }
        let mut older: Vec<(String, Value)> = Vec::new();
        let mut prev = delta["prev"].as_str().map(str::to_string);
        let base = loop {
            let Some(prev_id) = prev else {
                return Ok(None);
            };
            if let Some(request) = self.requests.get(&prev_id) {
                break request.clone();
            }
            let stored: Option<String> =
                sqlx::query_scalar("SELECT detail FROM agent_audit WHERE id = ?")
                    .bind(&prev_id)
                    .fetch_optional(pool)
                    .await?;
            let Some(Value::Object(mut stored)) =
                stored.and_then(|d| serde_json::from_str::<Value>(&d).ok())
            else {
                return Ok(None);
            };
            if let Some(request) = stored.remove("request") {
                let request = Arc::new(request);
                self.remember(prev_id, request.clone());
                break request;
            }
            let Some(delta) = stored.remove("request_delta") else {
                return Ok(None);
            };
            prev = delta["prev"].as_str().map(str::to_string);
            older.push((prev_id, delta));
        };
        let mut request = base;
        for (older_id, delta) in older.into_iter().rev() {
            request = Arc::new(apply_delta(&request, &delta));
            self.remember(older_id, request.clone());
        }
        let request = Arc::new(apply_delta(&request, delta));
        self.remember(id.to_string(), request.clone());
        Ok(Some(request))
    }

    fn remember(&mut self, id: String, request: Arc<Value>) {
        if self.requests.insert(id.clone(), request).is_some() {
            return;
        }
        self.order.push_back(id);
        if self.order.len() > CACHED_REQUESTS
            && let Some(oldest) = self.order.pop_front()
        {
            self.requests.remove(&oldest);
        }
    }
}

/// Put back every blob `detail` references in chain `chain`. A blob that is
/// gone, or whose content no longer matches its hash, stays a reference.
pub async fn resolve_blobs(pool: &Pool, chain: &str, detail: &mut Value) -> Result<(), DbError> {
    let mut wanted = Vec::new();
    walk_strings(detail, &mut |s| {
        if let Some(hash) = s.strip_prefix(BLOB_REF) {
            wanted.push(hash.to_string());
        }
    });
    if wanted.is_empty() {
        return Ok(());
    }
    let mut found: HashMap<String, String> = HashMap::new();
    for hash in wanted {
        if found.contains_key(&hash) {
            continue;
        }
        let data: Option<String> =
            sqlx::query_scalar("SELECT data FROM activity_blobs WHERE chain_key = ? AND hash = ?")
                .bind(chain)
                .bind(&hash)
                .fetch_optional(pool)
                .await?;
        if let Some(data) = data.filter(|d| sha256_hex(d.as_bytes()) == hash) {
            found.insert(hash, data);
        }
    }
    walk_strings(detail, &mut |s| {
        if let Some(data) = s.strip_prefix(BLOB_REF).and_then(|h| found.get(h)) {
            *s = data.clone();
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(messages: Value, tools: Option<Value>) -> Value {
        let mut r = json!({ "model": "m", "stream": true, "messages": messages });
        if let Some(t) = tools {
            r["tools"] = t;
        }
        r
    }

    #[test]
    fn a_delta_keeps_the_shared_messages_and_gives_back_the_request() {
        let sys = |s: &str| json!({ "role": "system", "content": s });
        let user = json!({ "role": "user", "content": "hi" });
        let call = json!({ "role": "assistant", "tool_calls": [{ "id": "c1" }] });
        let result = json!({ "role": "tool", "tool_call_id": "c1", "content": "ok" });
        let tools = json!([{ "function": { "name": "t" } }]);
        let first = request(json!([sys("a"), user]), Some(tools.clone()));
        let second = request(json!([sys("b"), user, call, result]), Some(tools));
        let delta = request_delta(&first, &second).unwrap();
        assert_eq!(delta["keep"], 2);
        assert_eq!(delta["messages"], json!([call, result]));
        assert_eq!(delta["system"], sys("b"));
        assert!(
            delta.get("tools").is_none(),
            "an unchanged offer is not stored"
        );
        assert_eq!(apply_delta(&first, &delta), second);

        let closing = request(json!([sys("b"), user, call, result]), None);
        let dropped = request_delta(&second, &closing).unwrap();
        assert_eq!(dropped["tools"], Value::Null);
        assert_eq!(apply_delta(&second, &dropped), closing);
    }

    #[test]
    fn the_reconstructor_lets_go_of_its_oldest_request_first() {
        let mut r = Reconstructor::default();
        for n in 0..=CACHED_REQUESTS {
            r.remember(format!("e{n}"), Arc::new(json!(n)));
        }
        r.remember("e1".into(), Arc::new(json!("again")));
        assert_eq!(r.requests.len(), CACHED_REQUESTS);
        assert_eq!(r.order.len(), CACHED_REQUESTS);
        assert!(!r.requests.contains_key("e0"), "the oldest went");
        assert_eq!(
            *r.requests[&format!("e{CACHED_REQUESTS}")],
            json!(CACHED_REQUESTS)
        );
    }

    #[test]
    fn a_request_that_shares_nothing_but_the_system_message_is_stored_whole() {
        let a = request(
            json!([{ "role": "system", "content": "s" }, { "role": "user", "content": "1" }]),
            None,
        );
        let b = request(
            json!([{ "role": "system", "content": "s" }, { "role": "user", "content": "2" }]),
            None,
        );
        assert_eq!(request_delta(&a, &b), None);
    }

    #[test]
    fn a_large_data_url_becomes_a_reference_and_a_small_one_stays() {
        let big = format!("data:image/png;base64,{}", "A".repeat(BLOB_MIN_BYTES));
        let small = "data:image/png;base64,iVBORw0KGgo=";
        let mut detail = json!({ "request": { "messages": [{ "content": [
            { "image_url": { "url": big } }, { "image_url": { "url": small } }
        ] }] } });
        let blobs = take_blobs(&mut detail);
        assert_eq!(blobs.len(), 1);
        assert_eq!(blobs[0].1, big);
        let parts = &detail["request"]["messages"][0]["content"];
        assert_eq!(
            parts[0]["image_url"]["url"],
            format!("{BLOB_REF}{}", blobs[0].0)
        );
        assert_eq!(parts[1]["image_url"]["url"], small);
    }
}
