// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Drift guard for the manual's exact registered-operation index.

use std::path::Path;

/// HTTP verbs the router registers (`with_get` → `GET`, …) and that the
/// API reference may prefix a path with (`GET /healthz`).
const METHODS: &[&str] = &["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"];

fn doc_covers(doc: &(Option<String>, String), route: &(String, String)) -> bool {
    doc.0.as_deref() == Some(route.0.as_str()) && doc.1 == route.1
}

/// Extract every `(METHOD, path)` route registered in `router.rs` by
/// scanning for `.with_<verb>("<path>"` builder calls. The path is always
/// the first string literal after the opening paren (handles the
/// multi-line registrations too).
fn actual_routes(src: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for verb in METHODS {
        let needle = format!(".with_{}(", verb.to_lowercase());
        let mut from = 0;
        while let Some(pos) = src[from..].find(&needle) {
            let after = from + pos + needle.len();
            from = after;
            let Some(q1) = src[after..].find('"') else {
                continue;
            };
            let s = after + q1 + 1;
            let Some(q2) = src[s..].find('"') else {
                continue;
            };
            out.push((verb.to_string(), src[s..s + q2].to_string()));
        }
    }
    out
}

/// Parse the API reference's "HTTP endpoints" table into documented
/// `(method?, path)` entries — every backtick token in each row's first
/// column.
fn documented_routes(reference: &str) -> Vec<(Option<String>, String)> {
    let start = reference
        .find("### HTTP endpoints")
        .expect("API reference is missing the `### HTTP endpoints` section");
    let mut out = Vec::new();
    for line in reference[start..].lines().skip(1) {
        let t = line.trim();
        if t.starts_with("## ") || t.starts_with("### ") {
            break; // next section
        }
        if !t.starts_with('|') {
            continue;
        }
        // First column: text between the first and second pipe.
        let cell = t.trim_matches('|').split('|').next().unwrap_or("");
        if cell.chars().all(|c| matches!(c, '-' | ':' | ' ')) {
            continue; // header separator row
        }
        for (i, part) in cell.split('`').enumerate() {
            if i % 2 == 0 {
                continue; // outside backticks
            }
            let tok = part.trim();
            if tok.is_empty() {
                continue;
            }
            match tok.split_once(' ') {
                Some((m, p)) if METHODS.contains(&m) => {
                    out.push((Some(m.to_string()), p.trim().to_string()))
                }
                _ => out.push((None, tok.to_string())),
            }
        }
    }
    out
}

#[test]
fn manual_http_endpoints_match_router() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let router_src = std::fs::read_to_string(manifest.join("src/rama_server/router.rs"))
        .expect("read router.rs");
    let reference = std::fs::read_to_string(manifest.join("../../docs/reference/api.md"))
        .expect("read docs/reference/api.md");

    let actual = actual_routes(&router_src);
    let docs = documented_routes(&reference);

    assert!(!actual.is_empty(), "parsed zero routes from router.rs");
    assert!(
        !docs.is_empty(),
        "parsed zero entries from the API reference table"
    );

    let mut errors = Vec::new();

    for route in &actual {
        if !docs.iter().any(|d| doc_covers(d, route)) {
            errors.push(format!(
                "  route `{} {}` is not in the API reference HTTP endpoints table \
                 (document the exact method and path)",
                route.0, route.1
            ));
        }
    }

    for doc in &docs {
        if !actual.iter().any(|r| doc_covers(doc, r)) {
            let m = doc.0.as_deref().unwrap_or("(any)");
            errors.push(format!(
                "  API reference documents `{} {}` but no such route exists in router.rs",
                m, doc.1
            ));
        }
    }

    assert!(
        errors.is_empty(),
        "API reference HTTP endpoints table is out of sync with router.rs:\n{}",
        errors.join("\n")
    );
}

#[test]
fn an_operation_index_entry_does_not_cover_an_undocumented_child() {
    let documented = (Some("GET".to_owned()), "/api/v0/chat".to_owned());
    assert!(!doc_covers(
        &documented,
        &("GET".to_owned(), "/api/v0/chat/sessions".to_owned())
    ));
    assert!(!doc_covers(
        &documented,
        &("POST".to_owned(), "/api/v0/chat".to_owned())
    ));
    assert!(doc_covers(
        &documented,
        &("GET".to_owned(), "/api/v0/chat".to_owned())
    ));
}
