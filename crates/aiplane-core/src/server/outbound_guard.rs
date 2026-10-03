// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The one way to connect to a destination a user, a model or an agent's
//! owner chose: `fetch_url`, `load_image_url` and `tls_cert` for the model,
//! an A2A route's card, endpoint and OAuth token URL and a `host_jwt`
//! verifier's JWKS URL for an agent's owner.
//!
//! Before every connection the host is resolved here, every address it
//! resolves to is checked, and the HTTP client is pinned to exactly those
//! addresses, so a second lookup (DNS rebinding) cannot swap a private
//! address in between the check and the request. The client never follows a
//! redirect itself: [`get`] follows them one hop at a time and puts every
//! hop through the same check, so a public page cannot bounce the gateway
//! into its own network.
//!
//! Always refused: unspecified, link-local (the cloud metadata address
//! `169.254.169.254` among them), broadcast and multicast addresses. Refused
//! unless the operator sets `$AIPLANE_ALLOW_PRIVATE_NETWORKS`
//! ([`PRIVATE_NETWORKS_VAR`]): loopback, private (RFC 1918, IPv6 unique
//! local) and carrier-grade NAT addresses.
//!
//! What an address is comes from `net_guard::classify`, shared with the
//! other outbound guards; the policy is this module's own. It is stricter
//! than `mcp_oauth::validate_outbound_url`, which allows private ranges on
//! purpose (an admin curates the MCP catalog). What comes back is read
//! through `capped_read`, so the peer does not decide how much is buffered.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use reqwest::Url;

use crate::server::config::PRIVATE_NETWORKS_VAR;
use crate::server::net_guard::{IpClass, classify};

/// How many redirects [`get`] follows before giving up.
pub const MAX_REDIRECTS: usize = 5;

/// Which schemes a destination may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Schemes {
    /// `https`; plain `http` only where private networks are allowed. For
    /// a peer that receives credentials or bound values.
    HttpsUnlessPrivate,
    /// `http` or `https`. For reading public web content.
    HttpOrHttps,
}

/// What a destination may be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub allow_private: bool,
    pub schemes: Schemes,
}

impl Policy {
    /// A URL an agent's owner names (A2A routes, JWKS).
    pub fn agent(allow_private: bool) -> Self {
        Self {
            allow_private,
            schemes: Schemes::HttpsUnlessPrivate,
        }
    }

    /// A URL a model asks the gateway to read.
    pub fn web(allow_private: bool) -> Self {
        Self {
            allow_private,
            schemes: Schemes::HttpOrHttps,
        }
    }
}

/// A host that passed the guard, and the client pinned to its addresses.
pub struct Pinned {
    pub url: Url,
    pub client: reqwest::Client,
}

/// Whether `ip` may be connected to, and why not.
pub fn check_ip(ip: IpAddr, allow_private: bool) -> Result<(), String> {
    let class = classify(ip);
    match class {
        IpClass::Public => Ok(()),
        IpClass::Unspecified | IpClass::LinkLocal | IpClass::Broadcast | IpClass::Multicast => Err(
            format!("{ip} is {}, which is never reached", class.describe()),
        ),
        IpClass::Loopback | IpClass::Private | IpClass::Cgnat if allow_private => Ok(()),
        IpClass::Loopback | IpClass::Private | IpClass::Cgnat => Err(format!(
            "{ip} is {}; a URL chosen by a user, a model or an agent's owner reaches public \
             hosts only, unless the operator sets `${PRIVATE_NETWORKS_VAR}=true`",
            class.describe()
        )),
    }
}

fn check_scheme(url: &Url, policy: Policy) -> Result<(), String> {
    match (url.scheme(), policy.schemes) {
        ("https", _) => Ok(()),
        ("http", Schemes::HttpOrHttps) => Ok(()),
        ("http", Schemes::HttpsUnlessPrivate) if policy.allow_private => Ok(()),
        ("http", Schemes::HttpsUnlessPrivate) => Err(format!(
            "{url} is plain http; this destination is reached over https only (plain http \
             only where the operator sets `${PRIVATE_NETWORKS_VAR}=true`)"
        )),
        (other, _) => Err(format!("{url} uses `{other}`; only http(s) is allowed")),
    }
}

/// Every address `host` resolves to, each one checked. A literal address is
/// checked as it is.
pub async fn resolve(
    host: &str,
    port: u16,
    allow_private: bool,
) -> Result<Vec<SocketAddr>, String> {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let addrs: Vec<SocketAddr> = match host.parse::<IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, port)],
        Err(_) => tokio::net::lookup_host((host, port))
            .await
            .map_err(|e| format!("cannot resolve `{host}` ({e})"))?
            .collect(),
    };
    if addrs.is_empty() {
        return Err(format!("`{host}` resolves to no address"));
    }
    for addr in &addrs {
        check_ip(addr.ip(), allow_private).map_err(|why| format!("`{host}` resolves to {why}"))?;
    }
    Ok(addrs)
}

/// Resolve `raw`, check every address, and build a client pinned to them
/// that follows no redirect.
pub async fn pin(raw: &str, policy: Policy, timeout: Duration) -> Result<Pinned, String> {
    let url = Url::parse(raw).map_err(|e| format!("`{raw}` is not a URL ({e})"))?;
    pin_url(url, policy, timeout).await
}

async fn pin_url(url: Url, policy: Policy, timeout: Duration) -> Result<Pinned, String> {
    check_scheme(&url, policy)?;
    let host = url
        .host_str()
        .ok_or_else(|| format!("`{url}` names no host"))?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let port = url
        .port_or_known_default()
        .ok_or_else(|| format!("`{url}` has no port"))?;
    let addrs = resolve(&host, port, policy.allow_private).await?;
    // The guarded client: pinned to the addresses checked above.
    #[allow(clippy::disallowed_methods)]
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .resolve_to_addrs(&host, &addrs)
        .build()
        .map_err(|e| format!("building the HTTP client failed: {e}"))?;
    Ok(Pinned { url, client })
}

/// `GET raw`, following up to [`MAX_REDIRECTS`] redirects, each hop resolved,
/// checked and pinned afresh. The answer is the last hop's, unread; its
/// `url()` is where it came from.
pub async fn get(
    raw: &str,
    policy: Policy,
    timeout: Duration,
    user_agent: &str,
) -> Result<reqwest::Response, String> {
    let mut url = Url::parse(raw).map_err(|e| format!("`{raw}` is not a URL ({e})"))?;
    for _ in 0..=MAX_REDIRECTS {
        let pinned = pin_url(url.clone(), policy, timeout).await?;
        let resp = pinned
            .client
            .get(pinned.url)
            .header(reqwest::header::USER_AGENT, user_agent)
            .send()
            .await
            .map_err(|e| format!("fetching {url} failed: {e}"))?;
        let location = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok());
        let Some(location) = location.filter(|_| resp.status().is_redirection()) else {
            return Ok(resp);
        };
        url = url
            .join(location)
            .map_err(|e| format!("{url} redirects to `{location}`, which is not a URL ({e})"))?;
    }
    Err(format!(
        "{raw} redirects more than {MAX_REDIRECTS} times; giving up"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    const T: Duration = Duration::from_secs(5);

    #[test]
    fn metadata_link_local_and_unspecified_are_never_reached() {
        for addr in [
            "169.254.169.254",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "fe80::1",
            "::",
            "ff02::1",
            "::ffff:169.254.169.254",
        ] {
            assert!(check_ip(ip(addr), true).is_err(), "{addr}");
            assert!(check_ip(ip(addr), false).is_err(), "{addr}");
        }
    }

    #[test]
    fn private_and_loopback_need_the_operator_switch() {
        for addr in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "100.64.0.1",
            "::1",
            "fd00::1",
            "::ffff:10.0.0.1",
            "::ffff:127.0.0.1",
        ] {
            let refused = check_ip(ip(addr), false).unwrap_err();
            assert!(refused.contains(PRIVATE_NETWORKS_VAR), "{refused}");
            assert!(check_ip(ip(addr), true).is_ok(), "{addr}");
        }
    }

    #[test]
    fn public_addresses_pass() {
        for addr in ["93.184.216.34", "2606:4700::1111", "100.128.0.1"] {
            assert!(check_ip(ip(addr), false).is_ok(), "{addr}");
        }
    }

    #[tokio::test]
    async fn the_agent_policy_wants_https_and_refuses_private_literals_before_connecting() {
        let http = pin("http://93.184.216.34/a2a", Policy::agent(false), T)
            .await
            .err()
            .unwrap();
        assert!(http.contains("https"), "{http}");
        let lo = pin("https://127.0.0.1:9/a2a", Policy::agent(false), T)
            .await
            .err()
            .unwrap();
        assert!(lo.contains("loopback"), "{lo}");
        let meta = pin("http://169.254.169.254/latest", Policy::agent(true), T)
            .await
            .err()
            .unwrap();
        assert!(meta.contains("link-local"), "{meta}");
        assert!(
            pin("http://127.0.0.1:9/a2a", Policy::agent(true), T)
                .await
                .is_ok()
        );
        assert!(
            pin("ftp://example.com/x", Policy::agent(true), T)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn the_web_policy_takes_plain_http_to_a_public_host() {
        assert!(
            pin("http://93.184.216.34/", Policy::web(false), T)
                .await
                .is_ok()
        );
        assert!(
            pin("file:///etc/passwd", Policy::web(true), T)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn the_gateways_own_network_is_refused_before_connecting() {
        for url in [
            "http://127.0.0.1:9/",
            "http://localhost:9/",
            "http://10.0.0.1/",
            "http://[::ffff:127.0.0.1]:9/",
            "http://169.254.169.254/latest/meta-data/",
        ] {
            let why = get(url, Policy::web(false), T, "t").await.unwrap_err();
            assert!(
                why.contains(PRIVATE_NETWORKS_VAR) || why.contains("never reached"),
                "{url}: {why}"
            );
        }
    }

    // wiremock listens on loopback, so these run with private networks
    // allowed; the hop the redirect lands on is link-local, which no switch
    // allows, so a refusal proves the hop was checked again.
    #[tokio::test]
    async fn a_redirect_is_checked_again_at_every_hop() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/bounce"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", "http://169.254.169.254/latest/meta-data/"),
            )
            .mount(&server)
            .await;
        let why = get(
            &format!("{}/bounce", server.uri()),
            Policy::web(true),
            T,
            "t",
        )
        .await
        .unwrap_err();
        assert!(why.contains("link-local"), "{why}");
    }

    #[tokio::test]
    async fn a_redirect_into_a_private_network_is_refused_without_the_switch() {
        // The first hop must be public to get as far as the redirect, and no
        // test peer is; check the second hop through the same path `get`
        // takes for it.
        let next = Url::parse("http://93.184.216.34/")
            .unwrap()
            .join("http://10.0.0.1/admin")
            .unwrap();
        let why = pin_url(next, Policy::web(false), T).await.err().unwrap();
        assert!(why.contains(PRIVATE_NETWORKS_VAR), "{why}");
    }

    #[tokio::test]
    async fn an_allowed_redirect_is_followed_to_its_answer() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/old"))
            .respond_with(ResponseTemplate::new(301).insert_header("location", "/new"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/new"))
            .respond_with(ResponseTemplate::new(200).set_body_string("here"))
            .mount(&server)
            .await;
        let resp = get(&format!("{}/old", server.uri()), Policy::web(true), T, "t")
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.url().path(), "/new");
    }

    #[tokio::test]
    async fn a_redirect_loop_gives_up() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/loop"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/loop"))
            .expect((MAX_REDIRECTS + 1) as u64)
            .mount(&server)
            .await;
        let why = get(&format!("{}/loop", server.uri()), Policy::web(true), T, "t")
            .await
            .unwrap_err();
        assert!(why.contains("redirects more than"), "{why}");
    }
}
