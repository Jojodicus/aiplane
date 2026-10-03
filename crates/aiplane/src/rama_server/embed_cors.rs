// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! CORS for the public agent endpoint `/api/v0/embed/*`, scoped to the
//! origins a live embed key lists. Lives with the router rather than beside
//! the `/v1` layer in `aiplane-core`: it reads embed keys, which are agent
//! data and sit above the base layer.

use std::convert::Infallible;
use std::sync::Arc;

use aiplane_core::server::db::Pool;
use aiplane_core::server::db::embed_keys::EmbeddableOrigins;
use rama::http::{Body, HeaderMap, HeaderValue, Method, Request, Response, StatusCode, header};
use rama::{Layer, Service};

/// [`Layer`] for CORS on the public agent endpoint, `/api/v0/embed/*`
/// (`docs/agents.md` §5). Every other `/api/v0` route stays same-origin.
///
/// Unlike `/v1`, the origin is never reflected blindly: it must be listed by
/// a live embed key of an enabled agent. A preflight carries neither the key
/// nor the visitor token, so this layer can only answer "does *some* key
/// allow this origin"; the handlers then check the origin against the
/// request's own key and refuse it with `origin_not_allowed`. The answer
/// comes from a cache ([`EmbeddableOrigins`]) that a key or agent change
/// clears at once.
#[derive(Clone, Debug)]
pub struct EmbedCorsLayer {
    pool: Pool,
    origins: Arc<EmbeddableOrigins>,
}

impl EmbedCorsLayer {
    pub fn new(pool: Pool) -> Self {
        Self {
            pool,
            origins: Arc::default(),
        }
    }
}

impl<S> Layer<S> for EmbedCorsLayer {
    type Service = EmbedCors<S>;

    fn layer(&self, inner: S) -> Self::Service {
        EmbedCors {
            inner,
            pool: self.pool.clone(),
            origins: self.origins.clone(),
        }
    }

    fn into_layer(self, inner: S) -> Self::Service {
        EmbedCors {
            inner,
            pool: self.pool,
            origins: self.origins,
        }
    }
}

/// See [`EmbedCorsLayer`].
#[derive(Clone, Debug)]
pub struct EmbedCors<S> {
    inner: S,
    pool: Pool,
    origins: Arc<EmbeddableOrigins>,
}

pub const EMBED_PREFIX: &str = "/api/v0/embed/";

/// The widget sends its visitor token as `Authorization` and its bodies as
/// JSON; it needs nothing else.
const EMBED_ALLOWED_REQUEST_HEADERS: &str = "authorization, content-type";

/// Short on purpose: a revoked key or a removed origin should stop working
/// in browsers within minutes, not after a day of cached preflights.
const EMBED_PREFLIGHT_MAX_AGE: &str = "600";

impl<S> Service<Request> for EmbedCors<S>
where
    S: Service<Request, Output = Response, Error = Infallible>,
{
    type Output = Response;
    type Error = Infallible;

    async fn serve(&self, req: Request) -> Result<Self::Output, Self::Error> {
        if !req.uri().path().starts_with(EMBED_PREFIX) {
            return self.inner.serve(req).await;
        }
        let Some(origin) = req.headers().get(header::ORIGIN).cloned() else {
            return self.inner.serve(req).await;
        };
        let allowed = match origin.to_str() {
            Ok(o) => self
                .origins
                .allows(&self.pool, o)
                .await
                .unwrap_or_else(|err| {
                    tracing::warn!(error = %err, "embed CORS: reading embed key origins");
                    false
                }),
            Err(_) => false,
        };

        if req.method() == Method::OPTIONS {
            let mut resp = Response::new(Body::empty());
            if allowed {
                *resp.status_mut() = StatusCode::NO_CONTENT;
                apply_embed_cors_headers(resp.headers_mut(), origin);
            } else {
                *resp.status_mut() = StatusCode::FORBIDDEN;
                resp.headers_mut()
                    .append(header::VARY, HeaderValue::from_static("origin"));
            }
            return Ok(resp);
        }

        let mut resp = self.inner.serve(req).await?;
        if allowed {
            apply_embed_cors_headers(resp.headers_mut(), origin);
        } else {
            resp.headers_mut()
                .append(header::VARY, HeaderValue::from_static("origin"));
        }
        Ok(resp)
    }
}

fn apply_embed_cors_headers(headers: &mut HeaderMap, origin: HeaderValue) {
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static(EMBED_ALLOWED_REQUEST_HEADERS),
    );
    headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static(EMBED_PREFLIGHT_MAX_AGE),
    );
    headers.append(header::VARY, HeaderValue::from_static("origin"));
}
