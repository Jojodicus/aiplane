// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The multipart bodies of the `/v1` upload routes (transcription, image
//! edits), taken apart and put back together around the VAD trim. The body
//! was already buffered behind `BodyLimitLayer`, so reading a field whole
//! here is bounded by the route's cap; that is why this module, and not
//! `proxy.rs`, is where the architecture test allows `.bytes()` on a field.

use rama::bytes::Bytes;
use rama::futures::stream;
use rama::http::HeaderMap;

/// A single parsed multipart field. We hold everything in memory — the
/// existing handler already buffered the whole body to extract `model`,
/// so this just makes the same buffering reusable for the rebuild.
pub(crate) struct MultipartField {
    pub name: String,
    pub filename: Option<String>,
    pub content_type: Option<String>,
    pub bytes: Bytes,
}

pub(crate) async fn parse_multipart_fields(
    headers: &HeaderMap,
    body: Bytes,
) -> Result<Vec<MultipartField>, String> {
    let ct = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            "missing Content-Type; transcription requires multipart/form-data".to_string()
        })?;
    let boundary = multer::parse_boundary(ct)
        .map_err(|e| format!("Content-Type is not a multipart/form-data: {e}"))?;
    let stream_once = stream::once(async move { Ok::<_, std::io::Error>(body) });
    let mut mp = multer::Multipart::new(stream_once, boundary);
    let mut fields = Vec::new();
    while let Some(field) = mp
        .next_field()
        .await
        .map_err(|e| format!("malformed multipart: {e}"))?
    {
        let name = field.name().unwrap_or("").to_string();
        let filename = field.file_name().map(str::to_owned);
        let content_type = field.content_type().map(|m| m.essence_str().to_string());
        let bytes = field
            .bytes()
            .await
            .map_err(|e| format!("reading multipart field `{name}`: {e}"))?;
        fields.push(MultipartField {
            name,
            filename,
            content_type,
            bytes,
        });
    }
    Ok(fields)
}

/// Serialise a parsed field set back into a multipart body. Returns the
/// body bytes and the matching `Content-Type` header value (boundary
/// included).
pub(crate) fn build_multipart(fields: &[MultipartField]) -> Result<(Bytes, String), String> {
    let boundary = format!("------rama-vad-{}", uuid::Uuid::new_v4().simple());
    let mut out: Vec<u8> = Vec::with_capacity(
        fields
            .iter()
            .map(|f| f.bytes.len() + f.name.len() + 64)
            .sum::<usize>()
            + boundary.len() * (fields.len() + 1),
    );
    for f in fields {
        // multer hands us the field name verbatim; we don't accept
        // arbitrary user input here (the chat composer + the API
        // client are the only writers), so a quote in the name is a
        // bug, not a security concern — reject loudly rather than
        // emit a malformed Content-Disposition.
        if f.name.contains('"') || f.name.contains('\r') || f.name.contains('\n') {
            return Err(format!(
                "multipart field name `{}` contains invalid characters",
                f.name
            ));
        }
        out.extend_from_slice(b"--");
        out.extend_from_slice(boundary.as_bytes());
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(b"Content-Disposition: form-data; name=\"");
        out.extend_from_slice(f.name.as_bytes());
        out.push(b'"');
        if let Some(fname) = f.filename.as_deref() {
            if fname.contains('"') || fname.contains('\r') || fname.contains('\n') {
                return Err(format!(
                    "multipart filename `{fname}` contains invalid characters"
                ));
            }
            out.extend_from_slice(b"; filename=\"");
            out.extend_from_slice(fname.as_bytes());
            out.push(b'"');
        }
        out.extend_from_slice(b"\r\n");
        if let Some(ct) = f.content_type.as_deref() {
            out.extend_from_slice(b"Content-Type: ");
            out.extend_from_slice(ct.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&f.bytes);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"--");
    out.extend_from_slice(boundary.as_bytes());
    out.extend_from_slice(b"--\r\n");
    let content_type = format!("multipart/form-data; boundary={boundary}");
    Ok((Bytes::from(out), content_type))
}
