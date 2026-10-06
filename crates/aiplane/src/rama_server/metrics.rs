// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! `GET /metrics` — the Prometheus scrape endpoint.
//!
//! Switched on and guarded from `/admin/settings` (the `metrics` card), read
//! from the effective configuration on every request so a saved change
//! applies to the next scrape. It is never open without a guard: switched on
//! with neither a token nor an allowed IP list, it answers 404 exactly as if
//! it were off.
//!
//! The body is the Prometheus text exposition format, written by hand: only
//! values the process already keeps — the upstream registry's per-backend
//! counters and the usage sink's dropped-record count — labelled by pool and
//! backend *name*, never by a URL, a key, a user or a model.

use std::fmt::Write as _;
use std::net::IpAddr;
use std::sync::Arc;

use rama::http::service::web::extract::State;
use rama::http::{HeaderValue, Request, Response, StatusCode, header};

use aiplane_core::server::config::MetricsConfig;
use aiplane_core::server::crypto::{constant_time_eq, sha256_hex};
use aiplane_core::server::ip_networks::IpNetworks;
use aiplane_core::server::upstreams::Pool;
use aiplane_runtime::rama_server::state::RamaState;

use crate::rama_server::pages::json_error;

const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

pub async fn scrape(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let config = state.config();
    let client_ip = state.client_ip(&req).and_then(|ip| ip.parse().ok());
    let authorization = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if let Err(refusal) = admit(&config.metrics, client_ip, authorization) {
        return refusal.response();
    }
    let body = render(
        &state.upstreams.pools(),
        state.usage.dropped(),
        aiplane_api::build_info::version(),
    );
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, CONTENT_TYPE)
        .body(body.into())
        .expect("static metrics response")
}

/// Why a scrape was turned away.
#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    Disabled,
    Unguarded,
    AddressNotAllowed(Option<IpAddr>),
    BadToken,
}

impl Refusal {
    fn response(&self) -> Response {
        match self {
            Self::Disabled => json_error(
                StatusCode::NOT_FOUND,
                "not_found",
                "GET /metrics is switched off on this gateway. An administrator turns it on \
                 under Settings → Access → Prometheus metrics.",
            ),
            Self::Unguarded => json_error(
                StatusCode::NOT_FOUND,
                "not_found",
                "GET /metrics is switched on but has neither a scrape token nor an allowed IP \
                 list, and it never answers unguarded. An administrator sets at least one under \
                 Settings → Access → Prometheus metrics.",
            ),
            Self::AddressNotAllowed(ip) => json_error(
                StatusCode::FORBIDDEN,
                "forbidden",
                &format!(
                    "GET /metrics refused the client address {}: it is not in the allowed IP \
                     list under Settings → Access → Prometheus metrics. Behind a reverse proxy, \
                     set AIPLANE_TRUSTED_PROXIES to the proxy so the gateway sees the real \
                     client.",
                    ip.map_or_else(|| "(unknown)".to_string(), |ip| ip.to_string())
                ),
            ),
            Self::BadToken => {
                let mut response = json_error(
                    StatusCode::UNAUTHORIZED,
                    "unauthorized",
                    "GET /metrics needs `Authorization: Bearer <token>` with the scrape token \
                     set under Settings → Access → Prometheus metrics; it was missing or wrong.",
                );
                response
                    .headers_mut()
                    .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
                response
            }
        }
    }
}

/// Whether a scrape from `client_ip` carrying `authorization` may read the
/// metrics. The address is checked before the token, so a client outside the
/// allowed networks learns nothing about the token.
fn admit(
    metrics: &MetricsConfig,
    client_ip: Option<IpAddr>,
    authorization: Option<&str>,
) -> Result<(), Refusal> {
    if !metrics.enabled {
        return Err(Refusal::Disabled);
    }
    let token = metrics.token.as_deref().filter(|t| !t.is_empty());
    if token.is_none() && metrics.allowed_ips.is_empty() {
        return Err(Refusal::Unguarded);
    }
    if !metrics.allowed_ips.is_empty() {
        let allowed = match IpNetworks::from_entries(metrics.allowed_ips.iter().map(String::as_str))
        {
            Ok(allowed) => allowed,
            Err(err) => {
                // The save refuses such an entry, so only a hand-edited row
                // gets here. Refusing everyone keeps the list a guard.
                tracing::error!(error = %err, "metrics.allowed_ips holds an unreadable entry; refusing every scrape until it is fixed in /admin/settings");
                IpNetworks::default()
            }
        };
        if !client_ip.is_some_and(|ip| allowed.contains(ip)) {
            return Err(Refusal::AddressNotAllowed(client_ip));
        }
    }
    if let Some(expected) = token {
        let presented = authorization
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim)
            .unwrap_or_default();
        if !constant_time_eq(
            sha256_hex(expected.as_bytes()).as_bytes(),
            sha256_hex(presented.as_bytes()).as_bytes(),
        ) {
            return Err(Refusal::BadToken);
        }
    }
    Ok(())
}

/// One metric family: its `# HELP` and `# TYPE` lines, then its samples.
struct Family<'a> {
    name: &'a str,
    kind: &'a str,
    help: &'a str,
}

impl Family<'_> {
    fn header(&self, out: &mut String) {
        let _ = writeln!(out, "# HELP {} {}", self.name, self.help);
        let _ = writeln!(out, "# TYPE {} {}", self.name, self.kind);
    }
}

/// The exposition-format body for the given pools, dropped-record count and
/// version.
fn render(pools: &[Arc<Pool>], usage_records_dropped: u64, version: &str) -> String {
    let mut out = String::new();

    Family {
        name: "aiplane_build_info",
        kind: "gauge",
        help: "The running build, as a label; the value is always 1.",
    }
    .header(&mut out);
    let _ = writeln!(
        out,
        "aiplane_build_info{{version=\"{}\"}} 1",
        escape_label(version)
    );

    let mut backends: Vec<(&str, &aiplane_core::server::upstreams::Backend)> = pools
        .iter()
        .flat_map(|pool| {
            pool.backends
                .iter()
                .map(move |b| (pool.name.as_str(), b.as_ref()))
        })
        .collect();
    backends.sort_by(|a, b| (a.0, &a.1.name).cmp(&(b.0, &b.1.name)));

    type Sample = fn(&aiplane_core::server::upstreams::Backend) -> u64;
    let per_backend: [(Family, Sample); 5] = [
        (
            Family {
                name: "aiplane_backend_up",
                kind: "gauge",
                help: "Whether the backend's last health probe succeeded (1) or not (0).",
            },
            |b| b.is_healthy().into(),
        ),
        (
            Family {
                name: "aiplane_backend_enabled",
                kind: "gauge",
                help: "Whether the backend may take traffic (1) or is switched off for maintenance (0).",
            },
            |b| b.is_enabled().into(),
        ),
        (
            Family {
                name: "aiplane_backend_auth_failed",
                kind: "gauge",
                help: "Whether the backend rejected the gateway's credentials on the last health probe (1) or not (0).",
            },
            |b| b.auth_failed().into(),
        ),
        (
            Family {
                name: "aiplane_backend_inflight",
                kind: "gauge",
                help: "Requests the gateway has in flight at the backend right now.",
            },
            |b| b.inflight().into(),
        ),
        (
            Family {
                name: "aiplane_backend_dispatched_total",
                kind: "counter",
                help: "Requests dispatched to the backend since it was loaded: at process start, or when a topology change was applied.",
            },
            |b| b.dispatched(),
        ),
    ];
    for (family, sample) in &per_backend {
        family.header(&mut out);
        for (pool, backend) in &backends {
            let _ = writeln!(
                out,
                "{}{{pool=\"{}\",backend=\"{}\"}} {}",
                family.name,
                escape_label(pool),
                escape_label(&backend.name),
                sample(backend)
            );
        }
    }

    Family {
        name: "aiplane_usage_records_dropped_total",
        kind: "counter",
        help: "Usage records dropped since the gateway process started because the usage writer could not keep up.",
    }
    .header(&mut out);
    let _ = writeln!(
        out,
        "aiplane_usage_records_dropped_total {usage_records_dropped}"
    );
    out
}

/// A label value as the exposition format requires: backslash, double quote
/// and line feed escaped.
fn escape_label(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use aiplane_core::server::upstreams::{
        BackendConfig, PickerStrategy, PoolKind, UpstreamPoolConfig, UpstreamRegistry,
    };

    use super::*;

    fn backend(name: &str) -> BackendConfig {
        BackendConfig {
            alias: None,
            supports_edit: false,
            enabled: true,
            name: name.into(),
            base_url: format!("http://{name}.internal:8000"),
            api_key_env: None,
            api_key: Some("sk-do-not-print".into()),
            weight: 1,
            max_inflight: 16,
            health_path: "/models".into(),
            models: Vec::new(),
        }
    }

    fn registry(pools: &[(&str, &[&str])]) -> Arc<UpstreamRegistry> {
        let configs: HashMap<String, UpstreamPoolConfig> = pools
            .iter()
            .map(|(name, backends)| {
                (
                    (*name).to_string(),
                    UpstreamPoolConfig {
                        voices: Default::default(),
                        offer_voices: Vec::new(),
                        allowed_groups: Vec::new(),
                        fallback_offline: None,
                        compliance: Default::default(),
                        enforce_limits: true,
                        kind: PoolKind::Chat,
                        strategy: PickerStrategy::RoundRobin,
                        models: vec!["m".into()],
                        backend: backends.iter().map(|b| backend(b)).collect(),
                    },
                )
            })
            .collect();
        UpstreamRegistry::new(&configs).unwrap()
    }

    fn sample<'a>(body: &'a str, prefix: &str) -> &'a str {
        body.lines()
            .find_map(|line| line.strip_prefix(prefix))
            .unwrap_or_else(|| panic!("no sample {prefix} in\n{body}"))
            .trim()
    }

    #[test]
    fn every_family_carries_help_and_type_before_its_samples() {
        let body = render(&registry(&[("chat", &["gpu0"])]).pools(), 0, "2610.1.0");
        for (name, kind) in [
            ("aiplane_build_info", "gauge"),
            ("aiplane_backend_up", "gauge"),
            ("aiplane_backend_enabled", "gauge"),
            ("aiplane_backend_auth_failed", "gauge"),
            ("aiplane_backend_inflight", "gauge"),
            ("aiplane_backend_dispatched_total", "counter"),
            ("aiplane_usage_records_dropped_total", "counter"),
        ] {
            let help = body
                .find(&format!("# HELP {name} "))
                .unwrap_or_else(|| panic!("{name} has no HELP"));
            let ty = body
                .find(&format!("# TYPE {name} {kind}\n"))
                .unwrap_or_else(|| panic!("{name} has no TYPE {kind}"));
            let first = body
                .lines()
                .position(|l| l.starts_with(name) && !l.starts_with('#'))
                .unwrap();
            let first_at: usize = body.lines().take(first).map(|l| l.len() + 1).sum();
            assert!(help < ty && ty < first_at, "{name} out of order:\n{body}");
        }
        assert!(body.ends_with('\n'));
    }

    #[test]
    fn backend_samples_come_from_the_live_registry() {
        let registry = registry(&[("chat", &["gpu0", "gpu1"]), ("embed", &["cpu"])]);
        let pools = registry.pools();
        let chat = pools.iter().find(|p| p.name == "chat").unwrap();
        let gpu1 = chat.backends.iter().find(|b| b.name == "gpu1").unwrap();
        gpu1.set_healthy(false);
        gpu1.set_auth_failed(true);
        let cpu = &pools.iter().find(|p| p.name == "embed").unwrap().backends[0];
        cpu.set_enabled(false);
        let held = chat.acquire_for_model("m").unwrap();
        drop(chat.acquire_for_model("m").unwrap());

        let body = render(&pools, 3, "2610.1.0");
        let gpu0 = r#"{pool="chat",backend="gpu0"}"#;
        let gpu1 = r#"{pool="chat",backend="gpu1"}"#;
        let cpu = r#"{pool="embed",backend="cpu"}"#;
        assert_eq!(sample(&body, &format!("aiplane_backend_up{gpu0}")), "1");
        assert_eq!(sample(&body, &format!("aiplane_backend_up{gpu1}")), "0");
        assert_eq!(
            sample(&body, &format!("aiplane_backend_auth_failed{gpu1}")),
            "1"
        );
        assert_eq!(
            sample(&body, &format!("aiplane_backend_auth_failed{gpu0}")),
            "0"
        );
        assert_eq!(sample(&body, &format!("aiplane_backend_enabled{cpu}")), "0");
        assert_eq!(
            sample(&body, &format!("aiplane_backend_enabled{gpu0}")),
            "1"
        );
        assert_eq!(
            sample(&body, &format!("aiplane_backend_inflight{gpu0}")),
            "1"
        );
        assert_eq!(
            sample(&body, &format!("aiplane_backend_dispatched_total{gpu0}")),
            "2"
        );
        assert_eq!(
            sample(&body, &format!("aiplane_backend_dispatched_total{cpu}")),
            "0"
        );
        assert_eq!(sample(&body, "aiplane_usage_records_dropped_total"), "3");
        assert_eq!(
            sample(&body, "aiplane_build_info"),
            r#"{version="2610.1.0"} 1"#
        );
        drop(held);
    }

    #[test]
    fn no_backend_url_or_key_is_exposed() {
        let body = render(&registry(&[("chat", &["gpu0"])]).pools(), 0, "v");
        assert!(!body.contains("internal:8000"), "{body}");
        assert!(!body.contains("sk-do-not-print"), "{body}");
    }

    #[test]
    fn label_values_are_escaped() {
        assert_eq!(escape_label(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(escape_label("line\nbreak"), r"line\nbreak");
        let body = render(&[], 0, "1\"2\n");
        assert!(
            body.contains(r#"aiplane_build_info{version="1\"2\n"} 1"#),
            "{body}"
        );
    }

    #[test]
    fn a_gateway_with_no_pools_still_reports_its_build_and_sink() {
        let body = render(&[], 0, "v");
        assert!(body.contains("# TYPE aiplane_backend_up gauge\n"));
        assert!(!body.contains("aiplane_backend_up{"));
        assert_eq!(sample(&body, "aiplane_usage_records_dropped_total"), "0");
    }

    fn config(enabled: bool, token: Option<&str>, ips: &[&str]) -> MetricsConfig {
        MetricsConfig {
            enabled,
            token: token.map(str::to_string),
            allowed_ips: ips.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn ip(s: &str) -> Option<IpAddr> {
        Some(s.parse().unwrap())
    }

    #[test]
    fn off_or_unguarded_is_not_found() {
        let any = Some("Bearer t");
        assert_eq!(
            admit(
                &config(false, Some("t"), &["0.0.0.0/0"]),
                ip("10.0.0.1"),
                any
            ),
            Err(Refusal::Disabled)
        );
        assert_eq!(
            admit(&config(true, None, &[]), ip("10.0.0.1"), any),
            Err(Refusal::Unguarded)
        );
        assert_eq!(
            admit(&config(true, Some(""), &[]), ip("10.0.0.1"), any),
            Err(Refusal::Unguarded),
            "an empty token is no token"
        );
    }

    #[test]
    fn a_token_alone_must_match_exactly() {
        let c = config(true, Some("s3cret"), &[]);
        assert_eq!(admit(&c, ip("203.0.113.9"), Some("Bearer s3cret")), Ok(()));
        for bad in [
            None,
            Some("Bearer"),
            Some("Bearer s3cre"),
            Some("Bearer s3cret2"),
            Some("Basic s3cret"),
            Some("s3cret"),
        ] {
            assert_eq!(
                admit(&c, ip("203.0.113.9"), bad),
                Err(Refusal::BadToken),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn an_ip_list_alone_admits_only_its_networks() {
        let c = config(true, None, &["10.0.0.0/8", "2001:db8::/32"]);
        assert_eq!(admit(&c, ip("10.2.3.4"), None), Ok(()));
        assert_eq!(admit(&c, ip("2001:db8::7"), None), Ok(()));
        assert_eq!(
            admit(&c, ip("192.0.2.1"), None),
            Err(Refusal::AddressNotAllowed(ip("192.0.2.1")))
        );
        assert_eq!(
            admit(&c, None, None),
            Err(Refusal::AddressNotAllowed(None)),
            "no known client address is not an allowed one"
        );
    }

    #[test]
    fn both_guards_must_pass_and_the_address_is_checked_first() {
        let c = config(true, Some("s3cret"), &["10.0.0.0/8"]);
        assert_eq!(admit(&c, ip("10.0.0.5"), Some("Bearer s3cret")), Ok(()));
        assert_eq!(
            admit(&c, ip("192.0.2.1"), Some("Bearer s3cret")),
            Err(Refusal::AddressNotAllowed(ip("192.0.2.1")))
        );
        assert_eq!(
            admit(&c, ip("10.0.0.5"), Some("Bearer wrong")),
            Err(Refusal::BadToken)
        );
        assert_eq!(
            admit(&c, ip("192.0.2.1"), Some("Bearer wrong")),
            Err(Refusal::AddressNotAllowed(ip("192.0.2.1")))
        );
    }

    #[test]
    fn an_unreadable_stored_list_refuses_everyone() {
        let c = config(true, Some("s3cret"), &["10.0.0.0/8", "not-a-network"]);
        assert_eq!(
            admit(&c, ip("10.0.0.5"), Some("Bearer s3cret")),
            Err(Refusal::AddressNotAllowed(ip("10.0.0.5")))
        );
    }

    #[test]
    fn refusals_answer_with_their_status_in_the_error_envelope() {
        for (refusal, status) in [
            (Refusal::Disabled, StatusCode::NOT_FOUND),
            (Refusal::Unguarded, StatusCode::NOT_FOUND),
            (Refusal::AddressNotAllowed(None), StatusCode::FORBIDDEN),
            (Refusal::BadToken, StatusCode::UNAUTHORIZED),
        ] {
            let response = refusal.response();
            assert_eq!(response.status(), status, "{refusal:?}");
            assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        }
        assert_eq!(
            Refusal::BadToken.response().headers()[header::WWW_AUTHENTICATE],
            "Bearer"
        );
    }
}
