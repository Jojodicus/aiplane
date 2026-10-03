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
//! the limit rather than the whole body. The A2A client and the RAG sources
//! read through it; the inbound counterpart is `session_core::chrome::read_body_capped`.

#[derive(Debug, thiserror::Error)]
pub enum CappedReadError {
    #[error("the body is larger than {max} bytes")]
    TooLarge { max: u64 },
    #[error("reading the body failed: {0}")]
    Transport(#[from] reqwest::Error),
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

/// [`read_capped`] with its errors worded for whoever reads the message;
/// `what` names the body (`the agent card`).
pub async fn read_capped_for(
    resp: reqwest::Response,
    max: usize,
    what: &str,
) -> Result<Vec<u8>, String> {
    read_capped(resp, max as u64).await.map_err(|e| match e {
        CappedReadError::TooLarge { .. } => format!("{what} is larger than {} KiB", max / 1024),
        CappedReadError::Transport(e) => format!("reading {what} failed: {e}"),
    })
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
}
