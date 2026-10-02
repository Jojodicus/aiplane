// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The public webhook trigger (`/hooks/{secret}`): whoever holds the URL
//! decides the body, so the gateway reads at most the payload cap of it.

use rama::http::{Method, Request, StatusCode};

use crate::common;

use aiplane_core::server::auth::token;
use aiplane_runtime::server::webhooks::{self, NewWebhook};

#[tokio::test]
async fn an_endless_body_is_cut_at_the_payload_cap_instead_of_buffered() {
    let state = common::state_no_skills().await;
    common::seed_user_with_token(&state, "owner").await;
    let (secret, secret_hash) = token::mint_webhook();
    let hook = webhooks::create(
        &state.db,
        NewWebhook {
            user_id: "owner".into(),
            name: "ci".into(),
            prompt: "summarise".into(),
            model: "model-a".into(),
            tools_enabled: false,
            synchronous: false,
            reuse_conversation: false,
            reuse_rounds: 0,
            secret_hash,
        },
    )
    .await
    .unwrap();

    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("/hooks/{secret}"))
        .body(common::endless_body())
        .unwrap();
    let resp = common::serve_promptly(&state, req).await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    let stored = webhooks::get(&state.db, "owner", &hook.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.last_payload.unwrap().len(), 256 * 1024);
}
