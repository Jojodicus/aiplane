// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! CORS for the OpenAI-compatible `/v1/*` API surface. The public agent
//! endpoint `/api/v0/embed/*` has its own, origin-checked layer in the
//! `aiplane` crate (`rama_server::embed_cors`).
//!
//! Browser-based apps (SPAs/PWAs) that call the gateway's `/v1` endpoints
//! directly need the server to (a) answer the CORS preflight
//! (`OPTIONS /v1/…`) *without* authentication and (b) echo the
//! `Access-Control-*` headers on every response — preflight, success,
//! error (4xx/5xx), and streaming (SSE) alike.
//!
//! This is a small purpose-built layer rather than rama's
//! [`rama::http::layer::cors`] for two reasons:
//!
//!   * **Scope.** Only the `/v1` API and the embed routes are cross-origin,
//!     and the embed routes have their own layer.
//!     The HTML UI, the OIDC `/auth/*` dance, and the rest of the
//!     session-cookie `/api/v0` surface are same-origin and are left
//!     untouched.
//!   * **Body type.** We only ever touch response *headers*, so the
//!     wrapped service's `Output` stays [`rama::http::Response`]
//!     (`Response<Body>`) — matching [`super::router::service`]'s
//!     signature and the test harness — instead of the
//!     `Response<OptionalBody<_>>` rama's `Cors` produces.
//!
//! Auth is a bearer token (in `Authorization` or its `x-api-key` spelling),
//! never a cookie, so credentials mode is not needed: we reflect the `Origin`
//! (falling back to `*` when none is sent, e.g. a non-browser client) and
//! deliberately do **not** emit `Access-Control-Allow-Credentials`.

use std::convert::Infallible;

use rama::http::{Body, HeaderMap, HeaderValue, Method, Request, Response, StatusCode, header};
use rama::{Layer, Service};

/// [`Layer`] that wraps a service with [`V1Cors`]. Apply it outside the
/// router's error handler so `RouterError`-rendered responses (404, 405,
/// …) on `/v1` paths are decorated too.
#[derive(Clone, Debug, Default)]
pub struct V1CorsLayer;

impl<S> Layer<S> for V1CorsLayer {
    type Service = V1Cors<S>;

    fn layer(&self, inner: S) -> Self::Service {
        V1Cors { inner }
    }

    fn into_layer(self, inner: S) -> Self::Service {
        V1Cors { inner }
    }
}

/// Adds CORS handling for `/v1/*`. See the [module docs](self).
#[derive(Clone, Debug)]
pub struct V1Cors<S> {
    inner: S,
}

impl<S> Service<Request> for V1Cors<S>
where
    S: Service<Request, Output = Response, Error = Infallible>,
{
    type Output = Response;
    type Error = Infallible;

    async fn serve(&self, req: Request) -> Result<Self::Output, Self::Error> {
        // Only the `/v1` API surface is cross-origin. Everything else (the
        // HTML UI, `/auth/*`, the session `/api/v0`) is same-origin and
        // passes through untouched.
        let path = req.uri().path();
        if !(path == "/v1" || path.starts_with("/v1/")) {
            return self.inner.serve(req).await;
        }

        // Reflect the caller's `Origin` so any https origin is allowed;
        // fall back to `*` when none is sent. Safe to echo because we never
        // allow credentials (auth is a bearer token, not a cookie).
        let origin = req
            .headers()
            .get(header::ORIGIN)
            .cloned()
            .unwrap_or_else(|| HeaderValue::from_static("*"));

        // Preflight: answer directly and never invoke the inner service —
        // the browser sends `OPTIONS` without the `Authorization` header,
        // so the preflight must not be gated on auth.
        if req.method() == Method::OPTIONS {
            return Ok(V1_CORS.preflight(origin));
        }

        // Actual request: run the handler, then decorate the response.
        // Header-only, so this works identically for JSON, error envelopes,
        // audio, and streaming (SSE) bodies.
        let mut resp = self.inner.serve(req).await?;
        V1_CORS.apply(resp.headers_mut(), origin);
        Ok(resp)
    }
}

/// The `/v1` answer.
///
/// `authorization` and `x-api-key` are the two spellings of the same gateway
/// token (see `rama_server::auth`), so allowing only the first would let an
/// Anthropic-format browser client authenticate in Node and fail in a page.
/// `anthropic-version` rides on *every* Messages API request, and
/// `anthropic-beta` is forwarded upstream deliberately — both are useless if
/// the preflight rejects them before the handler ever runs.
const V1_CORS: CorsHeaders = CorsHeaders {
    allow_headers: "authorization, content-type, x-api-key, anthropic-version, anthropic-beta",
    max_age_secs: "86400",
};

/// The `Access-Control-*` headers one cross-origin surface answers with. The
/// surfaces decide differently *which* origin to allow (`/v1` reflects any,
/// the embed endpoint only a live embed key's); what they write once they
/// allow one is this.
pub struct CorsHeaders {
    /// The request headers a cross-origin caller may set.
    pub allow_headers: &'static str,
    /// How long a browser may cache the preflight answer.
    pub max_age_secs: &'static str,
}

impl CorsHeaders {
    /// Allow `origin`: the four `Access-Control-*` headers, plus
    /// `Vary: Origin`, since the allow-origin value is derived from the
    /// request.
    pub fn apply(&self, headers: &mut HeaderMap, origin: HeaderValue) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, OPTIONS"),
        );
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static(self.allow_headers),
        );
        headers.insert(
            header::ACCESS_CONTROL_MAX_AGE,
            HeaderValue::from_static(self.max_age_secs),
        );
        vary_origin(headers);
    }

    /// The `204` that answers an allowed preflight, without running the
    /// handler: the browser sends `OPTIONS` without credentials.
    pub fn preflight(&self, origin: HeaderValue) -> Response {
        let mut resp = Response::new(Body::empty());
        *resp.status_mut() = StatusCode::NO_CONTENT;
        self.apply(resp.headers_mut(), origin);
        resp
    }
}

/// `Vary: Origin` — shared caches must key on the `Origin` request header
/// whenever the answer depends on it, allowed or not. `append`, so any
/// `Vary` an inner handler already set is kept.
pub fn vary_origin(headers: &mut HeaderMap) {
    headers.append(header::VARY, HeaderValue::from_static("origin"));
}
