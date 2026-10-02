// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The `verifiers` part of the spec validator (`docs/agents.md` "What #95
//! built"). Three kinds:
//!
//! - `mcp_code`: a one-time code the agent's own connector sends and checks;
//!   the gateway counts attempts, expiry and sends.
//! - `lookup`: a granted tool confirms what the visitor claimed (weaker).
//! - `host_jwt`: the embedding website signs who the visitor is.
//!
//! Shape is checked on every save; what a verifier needs to run (its
//! connector, the slots it reads and writes, its key) is required on publish.

use aiplane_core::server::principal::GrantKind;
use serde_json::{Map, Value};

use super::{Check, Stage, join, type_name};
use crate::agents::verifier::{
    self, CHECK_TOOL_DEFAULT, JwtAlgorithm, MAX_ATTEMPTS_CAP, MAX_CODE_TTL, MAX_LIFETIME_CAP,
    SEND_TOOL_DEFAULT,
};
use aiplane_core::server::db::agent_verifiers::MAX_WINDOW;

pub(super) const MCP_CODE: &str = "mcp_code";
pub(super) const LOOKUP: &str = "lookup";
pub(super) const HOST_JWT: &str = "host_jwt";
const VERIFIER_KINDS: &[&str] = &[MCP_CODE, LOOKUP, HOST_JWT];

const COMMON_KEYS: &[&str] = &["kind", "assurance"];
const MCP_CODE_KEYS: &[&str] = &[
    "connector",
    "send_tool",
    "check_tool",
    "input",
    "email_slot",
    "writes",
    "max_attempts",
    "code_ttl",
    "send_limits",
];
const LOOKUP_KEYS: &[&str] = &["tool", "inputs", "writes", "max_attempts"];
const HOST_JWT_KEYS: &[&str] = &[
    "algorithm",
    "secret",
    "secret_sealed",
    "public_key",
    "jwks_url",
    "issuer",
    "audience",
    "max_lifetime",
    "claims",
];
const SEND_LIMIT_SCOPES: &[&str] = &["email", "ip", "session"];
const INPUT_MODES: &[&str] = &["secure_field"];
const ALGORITHMS: &[&str] = &["HS256", "RS256", "ES256"];
/// HS256 with a shorter secret is guessable offline from one token.
const MIN_SECRET_BYTES: usize = 32;

fn keys_for(kind: Option<&str>) -> Vec<&'static str> {
    let own: &[&str] = match kind {
        Some(MCP_CODE) => MCP_CODE_KEYS,
        Some(LOOKUP) => LOOKUP_KEYS,
        Some(HOST_JWT) => HOST_JWT_KEYS,
        _ => &[],
    };
    let mut keys = COMMON_KEYS.to_vec();
    if kind.is_some_and(|k| VERIFIER_KINDS.contains(&k)) {
        keys.extend_from_slice(own);
    } else {
        keys.extend(MCP_CODE_KEYS.iter().chain(LOOKUP_KEYS).chain(HOST_JWT_KEYS));
        keys.sort_unstable();
        keys.dedup();
    }
    keys
}

impl Check<'_> {
    pub(super) fn verifier_kind(&self, id: &str) -> Option<&str> {
        self.verifiers.get(id).and_then(|k| k.as_deref())
    }

    /// Ids, kinds and everything that needs no slot: run before `state`, so
    /// `set_by: verifier:<id>` can be checked against what is declared.
    pub(super) fn verifiers_decl(&mut self, v: &Value) {
        let mut host_jwts = Vec::new();
        for (id, verifier) in self.named_map(v, "verifiers") {
            let p = join("verifiers", id);
            let kind = verifier
                .get("kind")
                .and_then(Value::as_str)
                .filter(|k| VERIFIER_KINDS.contains(k))
                .map(str::to_string);
            self.verifiers.insert(id.to_string(), kind.clone());
            let Some(map) = self.object(verifier, &p, &keys_for(kind.as_deref())) else {
                continue;
            };
            match map.get("kind") {
                Some(k) => {
                    self.one_of(k, &join(&p, "kind"), VERIFIER_KINDS);
                }
                None => self.issue(
                    &join(&p, "kind"),
                    format!(
                        "a verifier needs a `kind` — one of: {}",
                        VERIFIER_KINDS.join(", ")
                    ),
                ),
            }
            if let Some(a) = map.get("assurance") {
                self.assurance(a, &join(&p, "assurance"));
            }
            match kind.as_deref() {
                Some(MCP_CODE) => self.mcp_code(map, &p),
                Some(LOOKUP) => self.lookup(map, &p),
                Some(HOST_JWT) => {
                    host_jwts.push(p.clone());
                    self.host_jwt(map, &p);
                }
                _ => {}
            }
        }
        for extra in host_jwts.iter().skip(1) {
            self.issue(
                &join(extra, "kind"),
                format!(
                    "only one `host_jwt` verifier is allowed — the website presents one identity \
                     token, and `{}` already reads it",
                    host_jwts[0]
                ),
            );
        }
    }

    fn assurance(&mut self, v: &Value, path: &str) {
        if let Some(s) = self.string(v, path)
            && !super::is_ident(s)
        {
            self.issue(
                path,
                format!(
                    "`{s}` is not a valid label — use lowercase letters, digits and `_`, e.g. \
                     `low` or `email_verified`"
                ),
            );
        }
    }

    fn required(&mut self, map: &Map<String, Value>, path: &str, key: &str, why: &str) {
        if self.stage == Stage::Publish && !map.contains_key(key) {
            self.issue(&join(path, key), why.to_string());
        }
    }

    fn mcp_code(&mut self, map: &Map<String, Value>, p: &str) {
        match map.get("connector") {
            Some(c) => {
                let cp = join(p, "connector");
                if let Some(connector) = self.string(c, &cp) {
                    self.require_grant(&cp, GrantKind::Connector, connector, "connector");
                }
            }
            None => self.required(
                map,
                p,
                "connector",
                "an `mcp_code` verifier needs the `connector` that sends and checks the codes — \
                 one of this agent's connector grants",
            ),
        }
        for key in ["send_tool", "check_tool"] {
            if let Some(x) = map.get(key)
                && let Some(s) = self.string(x, &join(p, key))
                && s.trim().is_empty()
            {
                let default = if key == "send_tool" {
                    SEND_TOOL_DEFAULT
                } else {
                    CHECK_TOOL_DEFAULT
                };
                self.issue(
                    &join(p, key),
                    format!("name the connector's tool, or leave `{key}` out for `{default}`"),
                );
            }
        }
        if let Some(x) = map.get("input") {
            self.one_of(x, &join(p, "input"), INPUT_MODES);
        }
        if let Some(x) = map.get("max_attempts") {
            self.positive_int(
                x,
                &join(p, "max_attempts"),
                Some(u64::from(MAX_ATTEMPTS_CAP)),
            );
        }
        if let Some(x) = map.get("code_ttl") {
            self.bounded_duration(x, &join(p, "code_ttl"), Some(MAX_CODE_TTL));
        }
        if let Some(limits) = map.get("send_limits") {
            let lp = join(p, "send_limits");
            if let Some(limits) = self.object(limits, &lp, SEND_LIMIT_SCOPES) {
                for scope in SEND_LIMIT_SCOPES {
                    if let Some(rate) = limits.get(*scope) {
                        self.rate(rate, &join(&lp, scope), Some(MAX_WINDOW));
                    }
                }
            }
        }
        self.required(
            map,
            p,
            "email_slot",
            "an `mcp_code` verifier needs `email_slot`: the `email` slot whose address the code \
             is sent to",
        );
        self.required(
            map,
            p,
            "writes",
            "a verifier needs `writes`: which slots it sets once the code matched, e.g. \
             {\"verified\": \"result\"}",
        );
    }

    fn lookup(&mut self, map: &Map<String, Value>, p: &str) {
        match map.get("tool") {
            Some(t) => {
                let tp = join(p, "tool");
                if let Some(tool) = self.string(t, &tp) {
                    self.tool_reference(&tp, tool);
                }
            }
            None => self.required(
                map,
                p,
                "tool",
                "a `lookup` verifier needs the `tool` that confirms the visitor's details — a \
                 granted tool or `mcp__<connector>__<tool>`",
            ),
        }
        if let Some(x) = map.get("max_attempts") {
            self.positive_int(
                x,
                &join(p, "max_attempts"),
                Some(u64::from(MAX_ATTEMPTS_CAP)),
            );
        }
        self.required(
            map,
            p,
            "inputs",
            "a `lookup` verifier needs `inputs`: the tool's arguments, each `state.<slot>` or \
             {\"const\": …}",
        );
        self.required(
            map,
            p,
            "writes",
            "a verifier needs `writes`: which slots it sets once the lookup confirmed, e.g. \
             {\"verified\": \"result\"}",
        );
        self.required(
            map,
            p,
            "assurance",
            "a `lookup` verifier needs an `assurance` label (e.g. `low`): a lookup confirms what \
             the visitor knows, not who they are, and the label says so in the audit trail",
        );
    }

    fn host_jwt(&mut self, map: &Map<String, Value>, p: &str) {
        let alg = match map.get("algorithm") {
            Some(a) => self
                .one_of(a, &join(p, "algorithm"), ALGORITHMS)
                .and_then(|a| JwtAlgorithm::parse(&a)),
            None => {
                self.required(
                    map,
                    p,
                    "algorithm",
                    "a `host_jwt` verifier needs the `algorithm` the website signs with: HS256 \
                     (a shared `secret`), RS256 or ES256 (a `public_key` or `jwks_url`)",
                );
                None
            }
        };
        let secret = map.contains_key("secret") || map.contains_key("secret_sealed");
        let public = ["public_key", "jwks_url"]
            .iter()
            .filter(|k| map.contains_key(**k))
            .count();
        if map.contains_key("secret") && map.contains_key("secret_sealed") {
            self.issue(
                &join(p, "secret"),
                "give either a new `secret` or keep the stored `secret_sealed`, not both",
            );
        }
        if let Some(x) = map.get("secret")
            && let Some(s) = self.string(x, &join(p, "secret"))
            && s.len() < MIN_SECRET_BYTES
        {
            self.issue(
                &join(p, "secret"),
                format!(
                    "is too short to sign with — use a random secret of at least \
                     {MIN_SECRET_BYTES} characters"
                ),
            );
        }
        if let Some(x) = map.get("secret_sealed") {
            self.string(x, &join(p, "secret_sealed"));
        }
        match alg {
            Some(JwtAlgorithm::Hs256) if public > 0 => self.issue(
                &join(p, "algorithm"),
                "HS256 verifies with a shared `secret` — remove `public_key`/`jwks_url`, or use \
                 RS256/ES256",
            ),
            Some(JwtAlgorithm::Hs256) if !secret => self.required(
                map,
                p,
                "secret",
                "HS256 needs the `secret` the website signs with; it is sealed when saved",
            ),
            Some(JwtAlgorithm::Rs256 | JwtAlgorithm::Es256) if secret => self.issue(
                &join(p, "algorithm"),
                "RS256/ES256 verify with the website's public key — remove `secret`, or use HS256",
            ),
            Some(JwtAlgorithm::Rs256 | JwtAlgorithm::Es256) if public > 1 => self.issue(
                &join(p, "jwks_url"),
                "give either `public_key` or `jwks_url`, not both",
            ),
            Some(JwtAlgorithm::Rs256 | JwtAlgorithm::Es256)
                if public == 0 && self.stage == Stage::Publish =>
            {
                self.issue(
                    &join(p, "public_key"),
                    "RS256/ES256 need the website's `public_key` (PEM) or a `jwks_url` to \
                     fetch it from",
                );
            }
            _ => {}
        }
        if let (Some(alg), Some(x)) = (alg, map.get("public_key"))
            && let Some(pem) = self.string(x, &join(p, "public_key"))
            && let Err(err) = verifier::host_jwt::public_key(alg, pem)
        {
            self.issue(&join(p, "public_key"), err);
        }
        if let Some(x) = map.get("jwks_url")
            && let Some(url) = self.string(x, &join(p, "jwks_url"))
            && !verifier::host_jwt::is_jwks_url(url)
        {
            self.issue(
                &join(p, "jwks_url"),
                format!(
                    "`{url}` is not a JWKS address the gateway fetches — use an `https://` URL \
                     (plain `http://` only for localhost)"
                ),
            );
        }
        for (key, what) in [
            ("issuer", "the `iss` the website puts in its tokens"),
            (
                "audience",
                "the `aud` the website addresses this agent with",
            ),
        ] {
            match map.get(key) {
                Some(x) => {
                    if let Some(s) = self.string(x, &join(p, key))
                        && s.trim().is_empty()
                    {
                        self.issue(
                            &join(p, key),
                            format!("must not be empty — set it to {what}"),
                        );
                    }
                }
                None => self.required(
                    map,
                    p,
                    key,
                    &format!(
                        "a `host_jwt` verifier needs `{key}`: {what}. A token for another \
                         audience is refused"
                    ),
                ),
            }
        }
        if let Some(x) = map.get("max_lifetime") {
            self.bounded_duration(x, &join(p, "max_lifetime"), Some(MAX_LIFETIME_CAP));
        }
        self.required(
            map,
            p,
            "claims",
            "a `host_jwt` verifier needs `claims`: which slot each claim fills, e.g. \
             {\"verified\": {\"customer_id\": \"sub\"}}",
        );
    }

    /// What needs the declared slots: the email slot, the slots written and
    /// the slots a lookup reads.
    pub(super) fn verifier_refs(&mut self, v: &Value) {
        let Some(map) = v.as_object() else {
            return;
        };
        for (id, verifier) in map {
            let p = join("verifiers", id);
            let Some(cfg) = verifier.as_object() else {
                continue;
            };
            match self.verifier_kind(id).map(str::to_string).as_deref() {
                Some(MCP_CODE) => {
                    if let Some(x) = cfg.get("email_slot") {
                        self.email_slot(x, &join(&p, "email_slot"));
                    }
                    if let Some(x) = cfg.get("writes") {
                        self.writes(x, &join(&p, "writes"), id, &["email"]);
                    }
                }
                Some(LOOKUP) => {
                    let args = match cfg.get("inputs") {
                        Some(x) => self.lookup_inputs(x, &join(&p, "inputs")),
                        None => Vec::new(),
                    };
                    let args: Vec<&str> = args.iter().map(String::as_str).collect();
                    if let Some(x) = cfg.get("writes") {
                        self.writes(x, &join(&p, "writes"), id, &args);
                    }
                }
                Some(HOST_JWT) => {
                    if let Some(x) = cfg.get("claims") {
                        self.claims(x, &join(&p, "claims"));
                    }
                }
                _ => {}
            }
        }
    }

    fn slot_type(&self, slot: &str) -> Option<String> {
        self.schema
            .as_ref()
            .and_then(|s| s.slot(slot))
            .map(|d| d.ty.name().to_string())
    }

    fn email_slot(&mut self, v: &Value, path: &str) {
        let Some(slot) = self.string(v, path) else {
            return;
        };
        if !self.slots.contains_key(slot) {
            self.issue(
                path,
                format!("`{slot}` is not a slot in `state` — declare an `email` slot to send to"),
            );
        } else if self.slot_type(slot).is_some_and(|t| t != "email") {
            self.issue(
                path,
                format!("slot `{slot}` is not of type `email` — the code is sent to an address"),
            );
        }
    }

    /// `{slot: source}`: `result`, `result.<field>` or `input.<arg>`, into a
    /// slot this verifier may write.
    fn writes(&mut self, v: &Value, path: &str, id: &str, args: &[&str]) {
        let Value::Object(map) = v else {
            self.issue(
                path,
                format!("must be an object of slot → source, not {}", type_name(v)),
            );
            return;
        };
        if map.is_empty() {
            self.issue(
                path,
                "lists no slot — a verifier that writes nothing opens no gate",
            );
        }
        let inputs = args
            .iter()
            .map(|a| format!("`input.{a}`"))
            .collect::<Vec<_>>()
            .join(", ");
        for (slot, source) in map {
            let p = join(path, slot);
            self.verifier_target(&p, slot, &format!("verifier:{id}"));
            let Some(src) = self.string(source, &p) else {
                continue;
            };
            match verifier::WriteSource::parse(src) {
                Some(verifier::WriteSource::Input(arg)) if !args.contains(&arg.as_str()) => self
                    .issue(
                        &p,
                        format!(
                            "`{src}` names no input of this verifier — its inputs are: {}",
                            if inputs.is_empty() {
                                "none".into()
                            } else {
                                inputs.clone()
                            }
                        ),
                    ),
                Some(_) => {}
                None => self.issue(
                    &p,
                    format!(
                        "`{src}` is not a source — write `result` (the tool's whole answer), \
                         `result.<field>` or one of {}",
                        if inputs.is_empty() {
                            "no inputs".into()
                        } else {
                            inputs.clone()
                        }
                    ),
                ),
            }
        }
    }

    fn verifier_target(&mut self, path: &str, slot: &str, writer: &str) {
        let Some(def) = self.schema.as_ref().and_then(|s| s.slot(slot)) else {
            if !self.slots.contains_key(slot) {
                self.issue(
                    path,
                    format!(
                        "`{slot}` is not a slot in `state` — declare it, or write another slot"
                    ),
                );
            }
            return;
        };
        if !def.set_by.iter().any(|w| w.to_string() == writer) {
            self.issue(
                path,
                format!(
                    "slot `{slot}` does not list `{writer}` in its `set_by`, so this write would \
                     be refused — add `{writer}` to `state.{slot}.set_by`"
                ),
            );
        }
    }

    /// `{arg: "state.<slot>" | {"const": …}}`; the argument names.
    fn lookup_inputs(&mut self, v: &Value, path: &str) -> Vec<String> {
        let Value::Object(map) = v else {
            self.issue(
                path,
                format!(
                    "must be an object of argument → source, not {}",
                    type_name(v)
                ),
            );
            return Vec::new();
        };
        if map.is_empty() {
            self.issue(
                path,
                "lists no argument — a lookup needs something to look up",
            );
        }
        for (arg, source) in map {
            let p = join(path, arg);
            match source {
                Value::String(src) if src.starts_with("state.") => {
                    let slot = &src["state.".len()..];
                    if !self.slots.contains_key(slot) {
                        self.issue(
                            &p,
                            format!("reads `{src}`, but `{slot}` is not a slot in `state`"),
                        );
                    }
                }
                Value::Object(lit) if lit.len() == 1 && lit.contains_key("const") => {}
                other => self.issue(
                    &p,
                    format!(
                        "an input is `state.<slot>` (what the visitor told the agent) or \
                         {{\"const\": …}}, not {}",
                        match other {
                            Value::String(s) => format!("`{s}`"),
                            o => type_name(o).to_string(),
                        }
                    ),
                ),
            }
        }
        map.keys().cloned().collect()
    }

    /// `{slot: "<claim>" | {field: "<claim>"}}`, into slots the host may write.
    fn claims(&mut self, v: &Value, path: &str) {
        let Value::Object(map) = v else {
            self.issue(
                path,
                format!("must be an object of slot → claim, not {}", type_name(v)),
            );
            return;
        };
        if map.is_empty() {
            self.issue(path, "maps no claim — the token would set nothing");
        }
        for (slot, claim) in map {
            let p = join(path, slot);
            self.verifier_target(&p, slot, "host");
            match claim {
                Value::String(c) if !c.trim().is_empty() => {}
                Value::Object(fields) if !fields.is_empty() => {
                    if self.slot_type(slot).is_some_and(|t| t != "subject") {
                        self.issue(
                            &p,
                            format!(
                                "builds an object, but slot `{slot}` is not a `subject` — map one \
                                 claim to it instead"
                            ),
                        );
                    }
                    for (field, c) in fields {
                        if !c.as_str().is_some_and(|c| !c.trim().is_empty()) {
                            self.issue(
                                &join(&p, field),
                                "must name a claim of the token, e.g. `sub`",
                            );
                        }
                    }
                }
                _ => self.issue(
                    &p,
                    "maps a claim name (`\"sub\"`) or, for a `subject` slot, an object of field → \
                     claim ({\"customer_id\": \"sub\"})",
                ),
            }
        }
    }
}
