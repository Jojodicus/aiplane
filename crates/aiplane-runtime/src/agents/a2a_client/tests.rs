// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The A2A route's pure halves: reading a route, sealing it, the message
//! that leaves the gateway, and what a remote answer becomes. The exchange
//! itself runs against a wiremock A2A peer in `agents/run/tests/a2a.rs`.

use serde_json::json;

use super::*;

fn schema() -> Value {
    json!({ "type": "object", "required": ["answer"],
            "properties": { "answer": { "type": "string" } } })
}

fn finish() -> FinishContract {
    FinishContract::new(schema()).unwrap()
}

fn task(state: &str, parts: Value) -> Value {
    json!({ "task": {
        "id": "task-1",
        "contextId": "ctx-1",
        "status": { "state": state, "message": { "messageId": "m", "role": "ROLE_AGENT", "parts": parts } },
        "artifacts": [],
    } })
}

#[test]
fn a_completed_task_yields_its_data_artifact_checked_against_the_schema() {
    let mut done = task("TASK_STATE_COMPLETED", json!([]));
    done["task"]["artifacts"] = json!([{ "artifactId": "a1", "parts": [
        { "text": "here you go" },
        { "data": { "answer": "covered until 2027" }, "mediaType": "application/json" }
    ] }]);
    assert_eq!(
        interpret(&done, &finish()),
        Step::Done(RunOutcome::Finished {
            result: json!({ "answer": "covered until 2027" })
        })
    );
}

#[test]
fn json_text_counts_as_a_result_and_prose_does_not() {
    let fenced = json!({ "message": { "messageId": "m", "role": "ROLE_AGENT",
        "parts": [{ "text": "```json\n{\"answer\": \"yes\"}\n```" }] } });
    assert_eq!(
        interpret(&fenced, &finish()),
        Step::Done(RunOutcome::Finished {
            result: json!({ "answer": "yes" })
        })
    );
    let prose = json!({ "message": { "messageId": "m", "role": "ROLE_AGENT",
        "parts": [{ "text": "Yes, it is covered." }] } });
    let Step::Done(RunOutcome::Incomplete { reason, .. }) = interpret(&prose, &finish()) else {
        panic!("prose is no result");
    };
    assert!(
        matches!(reason, IncompleteReason::Failed { message } if message.contains("no structured result"))
    );
}

#[test]
fn a_result_violating_the_schema_ends_incomplete_with_the_violations() {
    let mut done = task("TASK_STATE_COMPLETED", json!([]));
    done["task"]["artifacts"] =
        json!([{ "artifactId": "a1", "parts": [{ "data": { "answer": 7 } }] }]);
    let Step::Done(RunOutcome::Incomplete { reason, .. }) = interpret(&done, &finish()) else {
        panic!("a violating result is incomplete");
    };
    let IncompleteReason::Failed { message } = reason else {
        panic!("failed");
    };
    assert!(message.contains("finish schema"), "{message}");
}

#[test]
fn input_required_pauses_only_for_structured_input() {
    assert_eq!(
        interpret(
            &task(
                "TASK_STATE_INPUT_REQUIRED",
                json!([
                    { "text": "Your order number?" },
                    { "data": { "type": "object", "properties": { "order": { "type": "string" } } } }
                ])
            ),
            &finish()
        ),
        Step::NeedsInput {
            task_id: "task-1".into(),
            context_id: Some("ctx-1".into())
        }
    );
    let Step::Done(RunOutcome::Incomplete { reason, .. }) = interpret(
        &task(
            "TASK_STATE_INPUT_REQUIRED",
            json!([{ "text": "Tell me more?" }]),
        ),
        &finish(),
    ) else {
        panic!("free-text input is incomplete");
    };
    assert!(
        matches!(reason, IncompleteReason::Failed { message } if message.contains("Tell me more?"))
    );
}

#[test]
fn failed_rejected_and_auth_required_end_incomplete_and_working_is_polled() {
    for state in [
        "TASK_STATE_FAILED",
        "TASK_STATE_REJECTED",
        "TASK_STATE_CANCELED",
        "TASK_STATE_AUTH_REQUIRED",
    ] {
        assert!(
            matches!(
                interpret(&task(state, json!([])), &finish()),
                Step::Done(RunOutcome::Incomplete { .. })
            ),
            "{state}"
        );
    }
    let working = interpret(&task("TASK_STATE_WORKING", json!([])), &finish());
    assert_eq!(
        working,
        Step::Working {
            task_id: "task-1".into()
        }
    );
    let get_task = json!({ "id": "task-1", "status": { "state": "TASK_STATE_COMPLETED" },
        "artifacts": [{ "artifactId": "a", "parts": [{ "data": { "answer": "ok" } }] }] });
    assert!(matches!(
        interpret(&get_task, &finish()),
        Step::Done(RunOutcome::Finished { .. })
    ));
}

#[test]
fn the_task_message_carries_the_task_and_the_bound_values_only() {
    let mut binds = BTreeMap::new();
    binds.insert("customer".to_string(), json!("K-1"));
    let message = task_message("Check order 42", &binds);
    assert_eq!(message["role"], "ROLE_USER");
    assert_eq!(
        message["parts"],
        json!([
            { "text": "Check order 42", "mediaType": "text/plain" },
            { "data": { "customer": "K-1" }, "mediaType": "application/json" }
        ])
    );
    let bare = task_message("Check order 42", &BTreeMap::new());
    assert_eq!(bare["parts"].as_array().unwrap().len(), 1);
}

#[test]
fn credentials_are_sealed_and_the_sealed_route_reads_back() {
    let crypto = aiplane_core::server::crypto::Crypto::ephemeral();
    let mut spec = json!({ "routes": {
        "partner": { "a2a": {
            "card_url": "https://p.example.com/.well-known/agent-card.json",
            "auth": { "kind": "bearer", "token": "secret-token-123" },
            "finish": { "schema": schema() },
            "budget": { "seconds": 30 }
        } },
        "oauth": { "a2a": {
            "card_url": "https://q.example.com/.well-known/agent-card.json",
            "auth": { "kind": "oauth_client_credentials", "client_id": "gw",
                      "client_secret": "client-secret-456", "scopes": ["tasks"] },
            "finish": { "schema": schema() }
        } }
    } });
    seal_secrets(&mut spec, &crypto).unwrap();
    let text = spec.to_string();
    assert!(!text.contains("secret-token-123") && !text.contains("client-secret-456"));
    let bearer = A2aTarget::from_route(&spec["routes"]["partner"]).unwrap();
    let auth = bearer.auth.unwrap();
    assert_eq!(auth.kind, AuthKind::Bearer);
    assert_eq!(
        crypto.open_from_string(&auth.secret_sealed).as_deref(),
        Some("secret-token-123")
    );
    assert_eq!(bearer.seconds, 30);
    let oauth = A2aTarget::from_route(&spec["routes"]["oauth"]).unwrap();
    assert_eq!(oauth.seconds, DEFAULT_SECONDS);
    let auth = oauth.auth.unwrap();
    assert_eq!(
        (auth.kind, auth.client_id.as_deref(), auth.scopes),
        (
            AuthKind::ClientCredentials,
            Some("gw"),
            vec!["tasks".to_string()]
        )
    );
}

#[test]
fn a_route_without_its_finish_schema_cannot_run() {
    let route = json!({ "a2a": { "card_url": "https://p.example.com/card" } });
    assert!(
        A2aTarget::from_route(&route)
            .unwrap_err()
            .contains("finish")
    );
}
