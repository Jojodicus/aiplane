// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The remote agent's card (A2A v1.0, section 8): fetched through the
//! [`guard`](super::guard), validated, and cached for [`CARD_TTL`].
//!
//! What the gateway needs from a card, and therefore checks:
//! - `name`;
//! - a `supportedInterfaces` entry with `protocolBinding: "JSONRPC"` and a
//!   `1.x` `protocolVersion` — the first such entry, as the spec orders them
//!   by preference — whose `url` is the endpoint, and its `tenant` if any;
//! - `capabilities` (an object);
//! - `securitySchemes`, read when the route brings credentials, and
//!   `securityRequirements`, which say whether the agent needs any.
//!
//! Anything else on the card (skills, modes, signatures) is not needed to
//! send a task and is not checked. Signed cards are not verified.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::guard;

/// How long a fetched card is reused.
pub const CARD_TTL: Duration = Duration::from_secs(5 * 60);
const CARD_TIMEOUT: Duration = Duration::from_secs(10);
/// A card larger than this is refused rather than parsed.
const MAX_CARD_BYTES: usize = 256 * 1024;

/// What the gateway uses of a card.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentCard {
    pub name: String,
    /// The JSON-RPC endpoint.
    pub endpoint: String,
    pub tenant: Option<String>,
    /// `securitySchemes`, by name, as the card declares them.
    pub security_schemes: serde_json::Map<String, Value>,
    /// Whether the card lists any security requirement.
    pub requires_auth: bool,
}

impl AgentCard {
    /// Read a card document, or say what is wrong with it.
    pub fn parse(doc: &Value) -> Result<Self, String> {
        let name = doc
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .ok_or("it has no `name`")?
            .to_string();
        if !doc.get("capabilities").is_some_and(Value::is_object) {
            return Err("it has no `capabilities` object".into());
        }
        let interfaces = doc
            .get("supportedInterfaces")
            .and_then(Value::as_array)
            .ok_or("it lists no `supportedInterfaces`")?;
        let chosen = interfaces
            .iter()
            .find(|i| {
                let binding = i.get("protocolBinding").and_then(Value::as_str);
                let version = i.get("protocolVersion").and_then(Value::as_str);
                binding.is_some_and(|b| b.eq_ignore_ascii_case("JSONRPC"))
                    && version.is_some_and(|v| v == "1" || v.starts_with("1."))
            })
            .ok_or(
                "it offers no JSON-RPC interface for A2A 1.x (`protocolBinding: \"JSONRPC\"`, \
                 `protocolVersion: \"1.0\"`), the only one the gateway speaks",
            )?;
        let endpoint = chosen
            .get("url")
            .and_then(Value::as_str)
            .filter(|u| !u.is_empty())
            .ok_or("its JSON-RPC interface has no `url`")?
            .to_string();
        let tenant = chosen
            .get("tenant")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .map(str::to_string);
        let security_schemes = match doc.get("securitySchemes") {
            None | Some(Value::Null) => serde_json::Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(_) => return Err("its `securitySchemes` is not an object".into()),
        };
        let requires_auth = doc
            .get("securityRequirements")
            .and_then(Value::as_array)
            .is_some_and(|r| !r.is_empty());
        Ok(Self {
            name,
            endpoint,
            tenant,
            security_schemes,
            requires_auth,
        })
    }
}

type Cache = Mutex<HashMap<String, (Instant, AgentCard)>>;
static CARDS: LazyLock<Cache> = LazyLock::new(Default::default);

/// The card at `card_url`, from the cache while it is fresh.
pub async fn fetch(card_url: &str, allow_private: bool) -> Result<AgentCard, String> {
    if let Some((at, card)) = CARDS.lock().ok().and_then(|c| c.get(card_url).cloned())
        && at.elapsed() < CARD_TTL
    {
        return Ok(card);
    }
    let pinned = guard::pin(card_url, allow_private, CARD_TIMEOUT).await?;
    let resp = pinned
        .client
        .get(pinned.url)
        .header("accept", "application/json")
        .send()
        .await
        .map_err(|e| format!("fetching the agent card failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("the agent card answered {status}"));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("reading the agent card failed: {e}"))?;
    if bytes.len() > MAX_CARD_BYTES {
        return Err(format!(
            "the agent card is larger than {} KiB",
            MAX_CARD_BYTES / 1024
        ));
    }
    let doc: Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("the agent card is not JSON ({e})"))?;
    let card = AgentCard::parse(&doc).map_err(|why| format!("the agent card is invalid: {why}"))?;
    if let Ok(mut cache) = CARDS.lock() {
        cache.insert(card_url.to_string(), (Instant::now(), card.clone()));
    }
    Ok(card)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn card() -> Value {
        json!({
            "name": "Partner",
            "description": "d",
            "version": "1.0.0",
            "supportedInterfaces": [
                { "url": "https://p.example.com/grpc", "protocolBinding": "GRPC", "protocolVersion": "1.0" },
                { "url": "https://p.example.com/old", "protocolBinding": "JSONRPC", "protocolVersion": "0.3" },
                { "url": "https://p.example.com/a2a", "protocolBinding": "JSONRPC", "protocolVersion": "1.0", "tenant": "t1" }
            ],
            "capabilities": { "streaming": false },
            "securitySchemes": { "bearer": { "httpAuthSecurityScheme": { "scheme": "Bearer" } } },
            "securityRequirements": [{ "schemes": { "bearer": { "list": [] } } }],
            "defaultInputModes": ["text/plain"],
            "defaultOutputModes": ["application/json"],
            "skills": []
        })
    }

    #[test]
    fn the_first_json_rpc_1x_interface_is_the_endpoint() {
        let parsed = AgentCard::parse(&card()).unwrap();
        assert_eq!(parsed.name, "Partner");
        assert_eq!(parsed.endpoint, "https://p.example.com/a2a");
        assert_eq!(parsed.tenant.as_deref(), Some("t1"));
        assert!(parsed.requires_auth);
        assert!(parsed.security_schemes.contains_key("bearer"));
    }

    #[test]
    fn a_card_without_what_a_task_needs_is_refused_with_the_reason() {
        let mut no_name = card();
        no_name["name"] = json!(" ");
        assert!(AgentCard::parse(&no_name).unwrap_err().contains("name"));
        let mut no_rpc = card();
        no_rpc["supportedInterfaces"] =
            json!([{ "url": "u", "protocolBinding": "GRPC", "protocolVersion": "1.0" }]);
        assert!(AgentCard::parse(&no_rpc).unwrap_err().contains("JSON-RPC"));
        let mut no_caps = card();
        no_caps.as_object_mut().unwrap().remove("capabilities");
        assert!(
            AgentCard::parse(&no_caps)
                .unwrap_err()
                .contains("capabilities")
        );
        let mut open = card();
        open.as_object_mut().unwrap().remove("securityRequirements");
        assert!(!AgentCard::parse(&open).unwrap().requires_auth);
    }
}
