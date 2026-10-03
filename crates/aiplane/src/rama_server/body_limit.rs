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
//! The router gives each group of routes its cap as the endpoint layer it
//! registers them under ([`endpoint`]), so a route's cap is where the route
//! is: [`BodyLimitLayer::DEFAULT`] for most, [`BodyLimitLayer::UPLOAD`] for
//! the routes that legitimately take large bodies (the model API with its
//! long contexts, base64 images and audio uploads; chat attachments;
//! transcription; feedback screenshots; skill archives), and
//! [`BodyLimitLayer::HANDLER_CAPPED`] for the public routes that read through
//! their own tighter cap (`/hooks`, `/a2a`, `/api/v0/embed`), passed through
//! untouched so their refusals keep their own shape. A route registered
//! under the handler-capped group must cap its own read; the architecture
//! test reads that group out of `router.rs` and checks the handlers.
//!
//! A refusal speaks the route's dialect: on the Anthropic Messages routes
//! (`/v1/messages`, `/v1/messages/count_tokens`, under
//! [`BodyLimitLayer::ANTHROPIC_UPLOAD`]) it is the Anthropic error envelope
//! (`request_too_large`) every other error there uses, so a client such as
//! Claude Code reads it like any other rejection; everywhere else it is the
//! gateway's OpenAI-shaped error.

use rama::http::service::web::router::DefaultEndpointLayer;
use rama::http::{Body, Request, Response, StatusCode};
use rama::{Layer, Service};
use session_core::chrome::{CappedBodyError, read_body_capped};

use crate::rama_server::{messages, proxy};

pub const DEFAULT_MAX_BODY_BYTES: usize = 1024 * 1024;
pub const UPLOAD_MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialect {
    OpenAi,
    Anthropic,
}

/// The body cap of one group of routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyLimitLayer {
    /// `None`: the handler reads through its own, tighter cap.
    max: Option<usize>,
    dialect: Dialect,
}

impl BodyLimitLayer {
    pub const DEFAULT: Self = Self {
        max: Some(DEFAULT_MAX_BODY_BYTES),
        dialect: Dialect::OpenAi,
    };
    pub const UPLOAD: Self = Self {
        max: Some(UPLOAD_MAX_BODY_BYTES),
        dialect: Dialect::OpenAi,
    };
    pub const ANTHROPIC_UPLOAD: Self = Self {
        max: Some(UPLOAD_MAX_BODY_BYTES),
        dialect: Dialect::Anthropic,
    };
    pub const HANDLER_CAPPED: Self = Self {
        max: None,
        dialect: Dialect::OpenAi,
    };
}

/// The endpoint layer a group of routes is registered under: its body cap in
/// front of rama's default endpoint layer.
pub type Endpoint = (BodyLimitLayer, DefaultEndpointLayer);

pub fn endpoint(limit: BodyLimitLayer) -> Endpoint {
    (limit, DefaultEndpointLayer::default())
}

/// A refusal of the body, in the route's dialect.
fn refusal(dialect: Dialect, status: StatusCode, code: &str, message: &str) -> Response {
    match dialect {
        Dialect::Anthropic => messages::error_response(status, message),
        Dialect::OpenAi => proxy::error_response(status, code, message),
    }
}

impl<S> Layer<S> for BodyLimitLayer {
    type Service = BodyLimit<S>;

    fn layer(&self, inner: S) -> Self::Service {
        BodyLimit {
            inner,
            limit: *self,
        }
    }
}

#[derive(Clone)]
pub struct BodyLimit<S> {
    inner: S,
    limit: BodyLimitLayer,
}

impl<S> Service<Request> for BodyLimit<S>
where
    S: Service<Request, Output = Response>,
{
    type Output = Response;
    type Error = S::Error;

    async fn serve(&self, req: Request) -> Result<Self::Output, Self::Error> {
        use rama::http::StreamingBody;
        let Some(max) = self.limit.max else {
            return self.inner.serve(req).await;
        };
        if req.body().size_hint().exact() == Some(0) {
            return self.inner.serve(req).await;
        }
        let (parts, body) = req.into_parts();
        let dialect = self.limit.dialect;
        match read_body_capped(body, max).await {
            Ok(bytes) => {
                self.inner
                    .serve(Request::from_parts(parts, Body::from(bytes)))
                    .await
            }
            Err(CappedBodyError::TooLarge { max }) => Ok(refusal(
                dialect,
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                &format!(
                    "the request body is larger than the {} MiB this endpoint accepts; send a \
                     smaller one",
                    max / (1024 * 1024)
                ),
            )),
            Err(CappedBodyError::Read(e)) => Ok(refusal(
                dialect,
                StatusCode::BAD_REQUEST,
                "invalid_request",
                &e,
            )),
        }
    }
}
