// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Reading an outbound response body without letting the peer decide how
//! much the gateway buffers.
//!
//! `resp.bytes()` buffers the whole body and only then lets the caller
//! compare, which is no bound at all against a peer that understates or omits
//! `Content-Length`. [`read_capped`] refuses a declared length over the cap
//! before reading anything, and otherwise reads chunk by chunk and stops the
//! moment the running total passes the cap, so the peak is one chunk over
//! the limit rather than the whole body. Every outbound response is read
//! through this module — the architecture test
//! `response_bodies_are_read_only_through_the_capped_reader` fails on a
//! `.bytes()` / `.text()` / `.json()` anywhere else. The inbound counterpart
//! is `session_core::chrome::read_body_capped`.
//!
//! Each caller picks the cap that fits what it reads; the shared ones are
//! below. An error answer is quoted, never parsed, so [`read_error_text`]
//! keeps its start and drops the rest instead of failing.

use serde::de::DeserializeOwned;

/// How much of an error answer is kept to quote in a message.
pub const ERROR_TEXT_BYTES: usize = 16 * 1024;

/// A small API answer: a token or discovery document, a search or lookup
/// result, a status probe.
pub const API_ANSWER_BYTES: u64 = 4 * 1024 * 1024;

/// A model backend's whole answer: a non-streamed completion, a batch of
/// embeddings, generated images inlined as base64.
pub const MODEL_ANSWER_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum CappedReadError {
    #[error("the body is larger than {max} bytes")]
    TooLarge { max: u64 },
    #[error("reading the body failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("the body is not the JSON expected: {0}")]
    Json(#[from] serde_json::Error),
}

pub async fn read_capped(
    mut resp: reqwest::Response,
    max: u64,
) -> Result<Vec<u8>, CappedReadError> {
    let declared = resp.content_length();
    if declared.is_some_and(|len| len > max) {
        return Err(CappedReadError::TooLarge { max });
    }
    let mut body = Vec::with_capacity(declared.unwrap_or(0) as usize);
    while let Some(chunk) = resp.chunk().await? {
        if body.len() as u64 + chunk.len() as u64 > max {
            return Err(CappedReadError::TooLarge { max });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// The body as text, decoded lossily as UTF-8.
pub async fn read_capped_text(
    resp: reqwest::Response,
    max: u64,
) -> Result<String, CappedReadError> {
    let body = read_capped(resp, max).await?;
    Ok(String::from_utf8(body).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into()))
}

/// The body parsed as JSON.
pub async fn read_capped_json<T: DeserializeOwned>(
    resp: reqwest::Response,
    max: u64,
) -> Result<T, CappedReadError> {
    Ok(serde_json::from_slice(&read_capped(resp, max).await?)?)
}

/// The start of an error answer, to quote in a message: at most
/// [`ERROR_TEXT_BYTES`], decoded lossily, empty when it cannot be read.
pub async fn read_error_text(mut resp: reqwest::Response) -> String {
    let mut body = Vec::new();
    while body.len() < ERROR_TEXT_BYTES {
        match resp.chunk().await {
            Ok(Some(chunk)) => body.extend_from_slice(&chunk),
            _ => break,
        }
    }
    body.truncate(ERROR_TEXT_BYTES);
    String::from_utf8_lossy(&body).into_owned()
}

/// [`read_capped`] with its errors worded for whoever reads the message;
/// `what` names the body (`the agent card`).
pub async fn read_capped_for(
    resp: reqwest::Response,
    max: usize,
    what: &str,
) -> Result<Vec<u8>, String> {
    read_capped(resp, max as u64)
        .await
        .map_err(|e| describe(e, max as u64, what))
}

/// [`read_capped_json`] with its errors worded like [`read_capped_for`]'s.
pub async fn read_capped_json_for<T: DeserializeOwned>(
    resp: reqwest::Response,
    max: u64,
    what: &str,
) -> Result<T, String> {
    read_capped_json(resp, max)
        .await
        .map_err(|e| describe(e, max, what))
}

/// `e` as a sentence about `what`.
pub fn describe(e: CappedReadError, max: u64, what: &str) -> String {
    match e {
        CappedReadError::TooLarge { .. } if max >= 1024 * 1024 => {
            format!("{what} is larger than {} MiB", max / (1024 * 1024))
        }
        CappedReadError::TooLarge { .. } => format!("{what} is larger than {} KiB", max / 1024),
        CappedReadError::Transport(e) => format!("reading {what} failed: {e}"),
        CappedReadError::Json(e) => format!("{what} is not the JSON expected: {e}"),
    }
}

#[cfg(test)]
// Tests build plain clients and drain bodies to talk to their in-process
// mocks; the outbound and body rules are about production paths.
#[allow(clippy::disallowed_methods)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// A peer that answers with a chunked body and no `Content-Length`: 8 MiB
    /// in 64 KiB chunks, far over any cap the tests set. A raw socket because
    /// wiremock always sends a length.
    ///
    /// Finite on purpose: were `read_capped` to lose its cap, a truly endless
    /// peer would have the test buffer until the machine runs out of memory
    /// (docs/dev-workflow.md → "Size-probe tests"); this one makes it fail
    /// with an `Ok` of 8 MiB instead.
    async fn endless_chunked_peer() -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let head = "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                        transfer-encoding: chunked\r\n\r\n";
            if sock.write_all(head.as_bytes()).await.is_err() {
                return;
            }
            let chunk = format!("10000\r\n{}\r\n", " ".repeat(0x10000));
            for _ in 0..128 {
                if sock.write_all(chunk.as_bytes()).await.is_err() {
                    return;
                }
            }
            let _ = sock.write_all(b"0\r\n\r\n").await;
        });
        format!("http://{addr}/")
    }

    async fn get(url: &str) -> reqwest::Response {
        reqwest::get(url).await.unwrap()
    }

    #[tokio::test]
    async fn a_body_without_a_length_is_cut_off_at_the_cap() {
        let url = endless_chunked_peer().await;
        let read = tokio::time::timeout(
            Duration::from_secs(10),
            read_capped(get(&url).await, 256 * 1024),
        )
        .await
        .expect("reading stopped at the cap instead of draining the stream");
        assert!(matches!(
            read,
            Err(CappedReadError::TooLarge { max: 262_144 })
        ));
    }

    #[tokio::test]
    async fn a_declared_length_over_the_cap_is_refused_and_one_within_is_read() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(2048)))
            .mount(&server)
            .await;
        let refused = read_capped(get(&server.uri()).await, 1024).await;
        assert!(matches!(
            refused,
            Err(CappedReadError::TooLarge { max: 1024 })
        ));
        let body = read_capped(get(&server.uri()).await, 2048).await.unwrap();
        assert_eq!(body.len(), 2048);
        let why = read_capped_for(get(&server.uri()).await, 1024, "the answer")
            .await
            .unwrap_err();
        assert!(why.contains("the answer is larger than 1 KiB"), "{why}");
    }

    #[tokio::test]
    async fn json_and_text_are_read_within_the_cap() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"a":1}"#))
            .mount(&server)
            .await;
        let url = server.uri();
        let v: serde_json::Value = read_capped_json(get(&url).await, 64).await.unwrap();
        assert_eq!(v["a"], 1);
        assert_eq!(
            read_capped_text(get(&url).await, 64).await.unwrap(),
            r#"{"a":1}"#
        );
        assert!(matches!(
            read_capped_json::<serde_json::Value>(get(&url).await, 4).await,
            Err(CappedReadError::TooLarge { max: 4 })
        ));
        let why = read_capped_json_for::<Vec<u8>>(get(&url).await, 64, "the list")
            .await
            .unwrap_err();
        assert!(why.contains("the list is not the JSON expected"), "{why}");
    }

    #[tokio::test]
    async fn an_error_answer_keeps_its_start_and_drops_the_rest() {
        let url = endless_chunked_peer().await;
        let text = tokio::time::timeout(Duration::from_secs(10), async {
            read_error_text(get(&url).await).await
        })
        .await
        .expect("reading stopped at the cap instead of draining the stream");
        assert_eq!(text.len(), ERROR_TEXT_BYTES);
    }
}
