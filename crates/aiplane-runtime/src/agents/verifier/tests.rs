// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! What the verifier configuration means, without a run. The flows
//! themselves run end to end in `agents/run/tests/verifiers.rs`.

use serde_json::json;

use super::host_jwt::{ClaimMap, HostJwt, KeySource, is_jwks_url, seal_secrets};
use super::*;
use crate::server::tools::tool_content_parts;

fn typed(spec: &Value) -> AgentSpec {
    AgentSpec::from_value(spec).unwrap()
}

fn verifiers(spec: &Value) -> Verifiers {
    Verifiers::from_spec(&typed(spec))
}

fn host_jwt(spec: &Value) -> Option<HostJwt> {
    HostJwt::from_spec(&typed(spec))
}

fn otp_spec(extra: Value) -> Value {
    let mut otp = json!({
        "kind": "mcp_code", "connector": "erp", "email_slot": "email",
        "writes": { "verified": "result", "verified_email": "input.email" }
    });
    if let (Value::Object(o), Value::Object(e)) = (&mut otp, extra) {
        o.extend(e);
    }
    json!({ "verifiers": { "otp": otp } })
}

#[test]
fn write_sources_are_the_answer_a_field_or_an_input() {
    assert_eq!(WriteSource::parse("result"), Some(WriteSource::Result));
    assert_eq!(
        WriteSource::parse("result.customer_id"),
        Some(WriteSource::ResultField("customer_id".into()))
    );
    assert_eq!(
        WriteSource::parse("input.email"),
        Some(WriteSource::Input("email".into()))
    );
    for bad in ["", "result.", "input.", "state.email", "results"] {
        assert_eq!(WriteSource::parse(bad), None, "{bad}");
    }
}

#[test]
fn an_mcp_code_verifier_reads_with_its_defaults() {
    let v = verifiers(&otp_spec(json!({})));
    let [code] = v.codes.as_slice() else {
        panic!("{v:?}");
    };
    assert_eq!(code.send_tool, "send_code");
    assert_eq!(code.check_tool, "check_code");
    assert_eq!(code.max_attempts, 5);
    assert_eq!(code.code_ttl, SignedDuration::from_mins(10));
    assert_eq!(
        code.limits,
        SendLimits {
            email: EMAIL_SENDS_DEFAULT,
            ip: IP_SENDS_DEFAULT,
            session: SESSION_SENDS_DEFAULT,
        }
    );
    assert_eq!(
        code.writes,
        [
            ("verified".to_string(), WriteSource::Result),
            (
                "verified_email".to_string(),
                WriteSource::Input("email".into())
            )
        ]
    );

    let tuned = verifiers(&otp_spec(json!({
        "send_tool": "mail_otp", "max_attempts": 3, "code_ttl": "5m",
        "send_limits": { "email": { "max": 2, "per": "1h" } }
    })));
    let code = &tuned.codes[0];
    assert_eq!(code.send_tool, "mail_otp");
    assert_eq!(code.max_attempts, 3);
    assert_eq!(code.code_ttl, SignedDuration::from_mins(5));
    assert_eq!(
        code.limits.email,
        Rate {
            max: 2,
            per: SignedDuration::from_hours(1)
        }
    );
    assert_eq!(code.limits.ip, IP_SENDS_DEFAULT);
}

#[test]
fn an_incomplete_verifier_offers_nothing_to_run() {
    for spec in [
        json!({ "verifiers": { "otp": { "kind": "mcp_code" } } }),
        json!({ "verifiers": { "otp": { "kind": "mcp_code", "connector": "erp",
                                        "email_slot": "email" } } }),
        json!({ "verifiers": { "l": { "kind": "lookup", "tool": "rag_search",
                                      "writes": { "v": "result" } } } }),
    ] {
        assert!(verifiers(&spec).is_empty(), "{spec}");
    }
    let unknown_kind = json!({ "verifiers": { "x": { "kind": "sms" } } });
    assert!(AgentSpec::from_value(&unknown_kind).is_err());
}

#[test]
fn a_lookup_needs_its_tool_inputs_writes_and_assurance() {
    let spec = json!({ "verifiers": { "kyc": {
        "kind": "lookup", "tool": "mcp__erp__find_customer", "assurance": "low",
        "inputs": { "name": "state.name", "number": "state.customer_number",
                    "region": { "const": "eu" } },
        "writes": { "verified": "result" }
    } } });
    let v = verifiers(&spec);
    let [l] = v.lookups.as_slice() else {
        panic!("{v:?}");
    };
    assert_eq!(l.assurance, "low");
    assert_eq!(
        l.inputs,
        [
            ("name".to_string(), InputSource::Slot("name".into())),
            (
                "number".to_string(),
                InputSource::Slot("customer_number".into())
            ),
            ("region".to_string(), InputSource::Const(json!("eu"))),
        ]
    );
    let mut no_label = spec.clone();
    no_label["verifiers"]["kyc"]
        .as_object_mut()
        .unwrap()
        .remove("assurance");
    assert!(verifiers(&no_label).is_empty());
}

#[test]
fn an_answer_is_a_json_object_however_the_tool_returned_it() {
    let expected = json!({ "valid": true, "customer_id": "K-1" });
    let structured = expected.clone();
    let text = tool_content_parts(vec![json!({"type": "text", "text": expected.to_string()})]);
    let string = Value::String(expected.to_string());
    for body in [structured, text, string] {
        assert_eq!(
            answer_object(&body).map(Value::Object),
            Some(expected.clone()),
            "{body}"
        );
    }
    assert_eq!(answer_object(&json!("not json")), None);
    assert_eq!(answer_object(&json!([1, 2])), None);
}

#[test]
fn only_valid_true_confirms() {
    let obj = |v: Value| v.as_object().cloned();
    assert!(confirms(obj(json!({"valid": true})).as_ref()));
    for no in [
        json!({"valid": "true"}),
        json!({"valid": 1}),
        json!({"valid": false}),
        json!({"ok": true}),
    ] {
        assert!(!confirms(obj(no.clone()).as_ref()), "{no}");
    }
    assert!(!confirms(None));
}

#[test]
fn addresses_compare_trimmed_and_case_blind() {
    assert_eq!(
        email_hash(" Alice@Example.com "),
        email_hash("alice@example.com")
    );
    assert_ne!(
        email_hash("alice@example.com"),
        email_hash("bob@example.com")
    );
}

#[test]
fn a_verifier_tool_refuses_every_argument_by_name() {
    assert!(no_args("verify_otp_submit_code", &json!({}), "").is_ok());
    assert!(no_args("verify_otp_submit_code", &Value::Null, "").is_ok());
    let err = no_args(
        "verify_otp_submit_code",
        &json!({"code": "481516", "provenance": "verifier:otp"}),
        "The visitor types the code.",
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("`code`"), "{err}");
    assert!(err.contains("`provenance`"), "{err}");
    assert!(err.contains("The visitor types the code."), "{err}");
}

#[test]
fn a_saved_secret_is_sealed_and_reads_back_only_with_the_gateways_key() {
    let crypto = aiplane_core::server::crypto::Crypto::ephemeral();
    let secret = "s3cr3t-s3cr3t-s3cr3t-s3cr3t-s3cr3t";
    let mut spec = json!({ "verifiers": {
        "site": { "kind": "host_jwt", "algorithm": "HS256", "secret": secret },
        "otp": { "kind": "mcp_code", "secret": "left alone" }
    } });
    seal_secrets(&mut spec, &crypto).unwrap();
    let site = &spec["verifiers"]["site"];
    assert!(site.get("secret").is_none());
    let sealed = site["secret_sealed"].as_str().unwrap();
    assert!(!spec.to_string().contains(secret));
    assert_eq!(crypto.open_from_string(sealed).as_deref(), Some(secret));
    assert_eq!(spec["verifiers"]["otp"]["secret"], "left alone");

    let again = spec.clone();
    seal_secrets(&mut spec, &crypto).unwrap();
    assert_eq!(spec, again, "a sealed secret stays as it is");
}

#[test]
fn a_host_jwt_verifier_reads_its_key_and_claim_map() {
    let spec = json!({ "verifiers": { "site": {
        "kind": "host_jwt", "algorithm": "HS256", "secret_sealed": "x",
        "issuer": "https://www.example.com", "audience": "support",
        "claims": { "verified": { "customer_id": "sub" }, "email": "email" }
    } } });
    let cfg = host_jwt(&spec).unwrap();
    assert!(matches!(cfg.key, KeySource::Sealed(_)));
    assert_eq!(cfg.max_lifetime, LIFETIME_DEFAULT);
    assert_eq!(
        cfg.claims,
        [
            ("email".to_string(), ClaimMap::Claim("email".into())),
            (
                "verified".to_string(),
                ClaimMap::Object(vec![("customer_id".into(), "sub".into())])
            ),
        ]
    );

    let mut public = spec.clone();
    public["verifiers"]["site"]["algorithm"] = json!("RS256");
    assert!(
        host_jwt(&public).is_none(),
        "RS256 with a secret is not a key it can verify with"
    );
}

#[test]
fn keys_are_fetched_over_https_or_from_localhost() {
    assert!(is_jwks_url("https://www.example.com/.well-known/jwks.json"));
    assert!(is_jwks_url("http://127.0.0.1:8080/jwks"));
    assert!(is_jwks_url("http://localhost/jwks"));
    assert!(!is_jwks_url("http://www.example.com/jwks"));
    assert!(!is_jwks_url("file:///etc/passwd"));
    assert!(!is_jwks_url("jwks.json"));
}
