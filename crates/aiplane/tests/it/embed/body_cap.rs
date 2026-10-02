// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The public embed routes read at most 64 KiB of a request body: a larger
//! declared body, or one that never ends, is refused with `413
//! payload_too_large` instead of being buffered.

use rama::http::{Body, Method, Request, StatusCode, header};
use serde_json::{Value, json};

use super::{SITE, embed_with};
use crate::common;

fn post(uri: &str, bearer: Option<&str>, body: Body) -> Request {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::ORIGIN, SITE)
        .header("content-type", "application/json");
    if let Some(b) = bearer {
        req = req.header(header::AUTHORIZATION, format!("Bearer {b}"));
    }
    req.body(body).unwrap()
}

async fn refused_as_too_large(resp: rama::http::Response) {
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body: Value = serde_json::from_slice(&common::read_body(resp).await).unwrap();
    assert_eq!(body["error"]["code"], "payload_too_large", "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("64 KiB"),
        "the message names the cap: {body}"
    );
}

#[tokio::test]
async fn starting_a_session_refuses_a_body_over_the_cap() {
    let e = embed_with(None, None).await;
    let big = json!({ "key": "k".repeat(65 * 1024) }).to_string();
    let resp = common::serve_promptly(
        &e.fx.state,
        post("/api/v0/embed/sessions", None, Body::from(big)),
    )
    .await;
    refused_as_too_large(resp).await;
}

#[tokio::test]
async fn starting_a_session_refuses_an_endless_body_without_buffering_it() {
    let e = embed_with(None, None).await;
    let resp = common::serve_promptly(
        &e.fx.state,
        post("/api/v0/embed/sessions", None, common::endless_body()),
    )
    .await;
    refused_as_too_large(resp).await;
}

#[tokio::test]
async fn a_visitors_routes_refuse_an_endless_body_without_buffering_it() {
    let e = embed_with(None, None).await;
    let token = e.visitor().await;
    for uri in [
        "/api/v0/embed/messages",
        "/api/v0/embed/resume",
        "/api/v0/embed/identity",
    ] {
        let resp =
            common::serve_promptly(&e.fx.state, post(uri, Some(&token), common::endless_body()))
                .await;
        refused_as_too_large(resp).await;
    }
}
