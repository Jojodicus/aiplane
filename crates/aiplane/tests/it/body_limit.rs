// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The router-wide request body cap (`rama_server::body_limit`): 1 MiB by
//! default, refused with `413 payload_too_large` before the handler runs;
//! the large-body routes take more. The cap sits in front of authentication,
//! so these requests carry no credentials: a capped body answers 413, a body
//! within the route's cap reaches the handler and its 401.

use rama::http::{Body, Method, Request, StatusCode, header};
use serde_json::Value;

use aiplane::rama_server::body_limit::UPLOAD_MAX_BODY_BYTES;

use crate::common;

fn post(uri: &str, body: Body) -> Request {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .unwrap()
}

fn over_the_default() -> Body {
    Body::from(vec![b' '; 1024 * 1024 + 1])
}

#[tokio::test]
async fn a_default_capped_route_refuses_a_body_over_one_mib() {
    let state = common::state_no_skills().await;
    for body in [over_the_default(), common::endless_body()] {
        let resp = common::serve_promptly(&state, post("/api/v0/tokens", body)).await;
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body: Value = serde_json::from_slice(&common::read_body(resp).await).unwrap();
        assert_eq!(body["error"]["code"], "payload_too_large", "{body}");
        assert!(
            body["error"]["message"].as_str().unwrap().contains("1 MiB"),
            "the message names the cap: {body}"
        );
    }
}

#[tokio::test]
async fn a_body_within_the_default_reaches_the_handler() {
    let state = common::state_no_skills().await;
    let resp = common::serve_promptly(&state, post("/api/v0/tokens", Body::from("{}"))).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_upload_route_takes_a_body_over_the_default() {
    let state = common::state_no_skills().await;
    for uri in ["/v1/chat/completions", "/api/v0/transcriptions"] {
        for body in [over_the_default(), common::endless_body()] {
            let resp = common::serve_promptly(&state, post(uri, body)).await;
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{uri}");
        }
    }
}

/// A finite body just past the upload cap, sent as 1 MiB chunks that share
/// one buffer and declare no length, so the cap trips on the running total.
fn over_the_upload_cap() -> Body {
    let chunk = rama::bytes::Bytes::from(vec![b' '; 1024 * 1024]);
    let chunks = UPLOAD_MAX_BODY_BYTES / (1024 * 1024) + 1;
    Body::from_stream(rama::futures::stream::iter(
        std::iter::repeat_n(chunk, chunks).map(Ok::<_, std::io::Error>),
    ))
}

#[tokio::test]
async fn the_messages_routes_refuse_an_oversized_body_in_the_anthropic_shape() {
    let state = common::state_no_skills().await;
    for uri in ["/v1/messages", "/v1/messages/count_tokens"] {
        let resp = common::serve_promptly(&state, post(uri, over_the_upload_cap())).await;
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE, "{uri}");
        let body: Value = serde_json::from_slice(&common::read_body(resp).await).unwrap();
        assert_eq!(body["type"], "error", "{uri}: {body}");
        assert_eq!(body["error"]["type"], "request_too_large", "{uri}: {body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("64 MiB"),
            "the message names the cap: {body}"
        );
    }
}

#[tokio::test]
async fn the_openai_routes_keep_their_shape_for_an_oversized_body() {
    let state = common::state_no_skills().await;
    let resp =
        common::serve_promptly(&state, post("/v1/chat/completions", over_the_upload_cap())).await;
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body: Value = serde_json::from_slice(&common::read_body(resp).await).unwrap();
    assert_eq!(body["error"]["code"], "payload_too_large", "{body}");
}
