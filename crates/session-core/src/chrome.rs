// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Cookie, body and response helpers shared by the gateway's HTTP surfaces.
//!
//! What is left after issue #22: the server renders almost nothing now, so
//! this is the small set of primitives the JSON API and the two OAuth
//! callback pages still need — reading a cookie, draining a request body,
//! a 303, and the SSE response wrapper the chat event stream builds on.

use rama::http::{Body, HeaderMap, Response, StatusCode, header};

// ---------------------------------------------------------------------------
// HTML escaping.

/// Escape the five HTML-significant characters (`& < > " '`) so a string
/// can be spliced into markup as inert text. Shared by every hand-built
/// HTML fragment that is assembled without a templating engine (e.g. the
/// gateway's OIDC form fields and the DB layer's search-snippet
/// highlighter) so the escape set can't drift between copies.
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Cookies.

/// Pull a named cookie out of a `Cookie:` header. Tolerates whitespace
/// after `;`; no percent-decoding (current callers store URL-safe
/// values only).
pub fn read_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let header = headers.get(header::COOKIE)?.to_str().ok()?;
    for piece in header.split(';') {
        let piece = piece.trim();
        if let Some((k, v)) = piece.split_once('=')
            && k == name
        {
            return Some(v.to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// SSE response helpers.

/// Bundle a set of pre-built SSE event payloads into a single response.
pub fn sse_response(events: &[rama::bytes::Bytes]) -> Response {
    let mut payload = Vec::with_capacity(events.iter().map(|e| e.len()).sum());
    for ev in events {
        payload.extend_from_slice(ev);
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("x-accel-buffering", "no")
        .body(payload.into())
        .unwrap()
}

// ---------------------------------------------------------------------------
// Body collection.

/// The whole body, uncapped. Only for handlers behind the router's
/// `BodyLimitLayer`, which has already buffered it up to the route's cap.
// The one uncapped reader; the architecture test keeps its callers behind the
// layer.
#[allow(clippy::disallowed_methods)]
pub async fn read_body_to_bytes(body: Body) -> Result<rama::bytes::Bytes, String> {
    use rama::http::body::util::BodyExt;
    body.collect()
        .await
        .map(|c| c.to_bytes())
        .map_err(|e| format!("reading body: {e}"))
}

/// Why [`read_body_capped`] gave up.
#[derive(Debug)]
pub enum CappedBodyError {
    /// The body is longer than the cap (declared or counted while reading).
    TooLarge {
        max: usize,
    },
    Read(String),
}

/// The body, refused once it passes `max` bytes. A declared length over the
/// cap is refused before anything is read; otherwise frames are counted as
/// they arrive and reading stops the moment the total passes the cap, so a
/// client cannot make the gateway hold more than `max` bytes of it.
pub async fn read_body_capped(
    body: Body,
    max: usize,
) -> Result<rama::bytes::Bytes, CappedBodyError> {
    use rama::http::StreamingBody;
    if body.size_hint().lower() > max as u64 {
        return Err(CappedBodyError::TooLarge { max });
    }
    match read_prefix(body, max).await? {
        (bytes, false) => Ok(bytes),
        (_, true) => Err(CappedBodyError::TooLarge { max }),
    }
}

/// The first `max` bytes of the body; the rest is never read. For payloads
/// that are stored truncated anyway (webhook triggers).
pub async fn read_body_prefix(body: Body, max: usize) -> Result<rama::bytes::Bytes, String> {
    match read_prefix(body, max).await {
        Ok((bytes, _)) => Ok(bytes),
        Err(CappedBodyError::Read(e)) => Err(e),
        Err(CappedBodyError::TooLarge { .. }) => unreachable!("read_prefix only reports reads"),
    }
}

/// `(at most max bytes, whether more was there)`.
async fn read_prefix(
    mut body: Body,
    max: usize,
) -> Result<(rama::bytes::Bytes, bool), CappedBodyError> {
    use rama::http::StreamingBody;
    use rama::http::body::util::BodyExt;
    let declared = body.size_hint().lower();
    let mut buf = Vec::with_capacity((declared as usize).min(max));
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|e| CappedBodyError::Read(format!("reading body: {e}")))?;
        let Ok(data) = frame.into_data() else {
            continue;
        };
        let room = max - buf.len();
        if data.len() > room {
            buf.extend_from_slice(&data[..room]);
            return Ok((buf.into(), true));
        }
        buf.extend_from_slice(&data);
    }
    Ok((buf.into(), false))
}

// ---------------------------------------------------------------------------
// Plain (unauthed) HTML responses.

/// 303 redirect — Post/Redirect/Get so reloads don't re-submit.
pub fn see_other(to: &str) -> Response {
    Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, to)
        .body("".into())
        .unwrap()
}
