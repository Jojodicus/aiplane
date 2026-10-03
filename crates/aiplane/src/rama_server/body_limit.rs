// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The request body cap every route sits behind.
//!
//! Most handlers drain their whole body before looking at it, so without a
//! cap here any client could make the gateway buffer as much as it cares to
//! send. [`BodyLimitLayer`] reads the body itself, up to the route's limit,
//! and hands the handler the buffered bytes: a declared length over the
//! limit is refused before anything is read, a body without one is refused
//! the moment the running total passes it, and both answer `413
//! payload_too_large` naming the limit. Handlers stay unchanged — what they
//! drain is already bounded.
//!
//! The limit is [`DEFAULT_MAX_BODY_BYTES`] unless the path is listed in
//! [`policy`]: the routes that legitimately take large bodies (the model API
//! with its long contexts, base64 images and audio uploads; chat attachments;
//! transcription; feedback screenshots; skill archives) get
//! [`UPLOAD_MAX_BODY_BYTES`], and the public routes that already read through
//! their own tighter cap (`/hooks`, `/a2a`, `/api/v0/embed`) are passed
//! through untouched so their refusals keep their own shape. A new route
//! under one of those pass-through prefixes must cap its own read.
//!
//! A refusal speaks the route's dialect: on the Anthropic Messages routes
//! (`/v1/messages`, `/v1/messages/count_tokens`) it is the Anthropic error
//! envelope (`request_too_large`) every other error there uses, so a client
//! such as Claude Code reads it like any other rejection; everywhere else it
//! is the gateway's OpenAI-shaped error.

use std::convert::Infallible;

use rama::http::{Body, Request, Response, StatusCode};
use rama::{Layer, Service};
use session_core::chrome::{CappedBodyError, read_body_capped};

use crate::rama_server::{messages, proxy};

pub const DEFAULT_MAX_BODY_BYTES: usize = 1024 * 1024;
pub const UPLOAD_MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

const UPLOAD_PREFIXES: &[&str] = &[
    "/v1/",
    "/api/v0/chat/",
    "/api/v0/transcriptions",
    "/api/v0/feedback",
    "/api/v0/skills",
    "/api/v0/admin/skills",
];

pub const HANDLER_CAPPED_PREFIXES: &[&str] = &["/hooks/", "/a2a/", "/api/v0/embed/"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    Cap(usize),
    /// The handler reads through its own, tighter cap.
    HandlerCapped,
}

/// Whether `path` is an Anthropic Messages route, whose errors use the
/// Anthropic envelope.
fn is_anthropic(path: &str) -> bool {
    path.strip_prefix("/v1/messages")
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// A refusal of the body, in the dialect of the route at `path`.
fn refusal(path: &str, status: StatusCode, code: &str, message: &str) -> Response {
    if is_anthropic(path) {
        messages::error_response(status, message)
    } else {
        proxy::error_response(status, code, message)
    }
}

pub fn policy(path: &str) -> Policy {
    if HANDLER_CAPPED_PREFIXES.iter().any(|p| path.starts_with(p)) {
        Policy::HandlerCapped
    } else if UPLOAD_PREFIXES.iter().any(|p| path.starts_with(p)) {
        Policy::Cap(UPLOAD_MAX_BODY_BYTES)
    } else {
        Policy::Cap(DEFAULT_MAX_BODY_BYTES)
    }
}

#[derive(Clone, Copy, Default)]
pub struct BodyLimitLayer;

impl<S> Layer<S> for BodyLimitLayer {
    type Service = BodyLimit<S>;

    fn layer(&self, inner: S) -> Self::Service {
        BodyLimit { inner }
    }
}

#[derive(Clone)]
pub struct BodyLimit<S> {
    inner: S,
}

impl<S> Service<Request> for BodyLimit<S>
where
    S: Service<Request, Output = Response, Error = Infallible>,
{
    type Output = Response;
    type Error = Infallible;

    async fn serve(&self, req: Request) -> Result<Self::Output, Self::Error> {
        use rama::http::StreamingBody;
        let Policy::Cap(max) = policy(req.uri().path()) else {
            return self.inner.serve(req).await;
        };
        if req.body().size_hint().exact() == Some(0) {
            return self.inner.serve(req).await;
        }
        let (parts, body) = req.into_parts();
        let path = parts.uri.path();
        match read_body_capped(body, max).await {
            Ok(bytes) => {
                self.inner
                    .serve(Request::from_parts(parts, Body::from(bytes)))
                    .await
            }
            Err(CappedBodyError::TooLarge { max }) => Ok(refusal(
                path,
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                &format!(
                    "the request body is larger than the {} MiB this endpoint accepts; send a \
                     smaller one",
                    max / (1024 * 1024)
                ),
            )),
            Err(CappedBodyError::Read(e)) => Ok(refusal(
                path,
                StatusCode::BAD_REQUEST,
                "invalid_request",
                &e,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_body_routes_get_the_upload_cap_and_the_rest_the_default() {
        for path in [
            "/v1/chat/completions",
            "/v1/messages",
            "/v1/audio/transcriptions",
            "/v1/images/edits",
            "/api/v0/chat/sessions/s1/messages",
            "/api/v0/transcriptions",
            "/api/v0/feedback",
            "/api/v0/feedback/extract",
            "/api/v0/skills",
            "/api/v0/admin/skills",
        ] {
            assert_eq!(policy(path), Policy::Cap(UPLOAD_MAX_BODY_BYTES), "{path}");
        }
        for path in [
            "/api/v0/tokens",
            "/api/v0/admin/settings",
            "/api/v0/rag/collections",
            "/api/v0/agents/a1/draft",
            "/auth/logout",
        ] {
            assert_eq!(policy(path), Policy::Cap(DEFAULT_MAX_BODY_BYTES), "{path}");
        }
    }

    #[test]
    fn only_the_messages_routes_speak_the_anthropic_dialect() {
        for path in ["/v1/messages", "/v1/messages/count_tokens"] {
            assert!(is_anthropic(path), "{path}");
        }
        for path in [
            "/v1/chat/completions",
            "/v1/messagesx",
            "/api/v0/chat/messages",
        ] {
            assert!(!is_anthropic(path), "{path}");
        }
    }

    #[test]
    fn routes_with_their_own_cap_are_passed_through() {
        for path in [
            "/hooks/secret",
            "/hooks/rag/token",
            "/a2a/agents/a1",
            "/api/v0/embed/messages",
        ] {
            assert_eq!(policy(path), Policy::HandlerCapped, "{path}");
        }
    }
}
