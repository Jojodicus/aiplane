// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Where an agent may connect (`docs/agents.md` "What #101 built"): an A2A
//! route's card, endpoint and OAuth token URL, and a `host_jwt` verifier's
//! JWKS address.
//!
//! A spec names the card URL and the JWKS URL, and the card names the
//! endpoint, so all come from outside the gateway's own configuration. Before every connection the
//! host is resolved here, every address it resolves to is checked, and the
//! HTTP client is pinned to exactly those addresses, so a second lookup
//! (DNS rebinding) cannot swap a private address in between the check and
//! the request. Redirects are never followed. What comes back is read
//! through [`read_capped`], so no peer decides how much the gateway buffers.
//!
//! Always refused: unspecified, link-local (the cloud metadata address
//! `169.254.169.254` among them), broadcast and multicast addresses. Refused
//! unless `$AIPLANE_A2A_ALLOW_PRIVATE_NETWORKS` is on: loopback, private
//! (RFC 1918, IPv6 unique local), carrier-grade NAT and plain `http`.
//!
//! What an address is comes from `net_guard::classify`, shared with the
//! other outbound guards; the policy is this module's own. It is stricter
//! than `mcp_oauth::validate_outbound_url`, which allows private ranges on
//! purpose (an admin curates the MCP catalog), while a route's target is
//! chosen by an agent's owner and must not reach the gateway's own network.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use aiplane_core::server::capped_read::{self, CappedReadError};
use aiplane_core::server::net_guard::{IpClass, classify, is_loopback_host};
use reqwest::Url;

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
            "{ip} is {}; an agent reaches public hosts only unless the operator sets \
             `$AIPLANE_A2A_ALLOW_PRIVATE_NETWORKS=true`",
            class.describe()
        )),
    }
}

/// The scheme rule: `https`, or `http` where private networks are allowed.
fn check_scheme(url: &Url, allow_private: bool) -> Result<(), String> {
    match url.scheme() {
        "https" => Ok(()),
        "http" if allow_private => Ok(()),
        "http" => Err(format!(
            "{url} is plain http; an agent reaches other hosts over https only (plain http \
             only where the operator sets `$AIPLANE_A2A_ALLOW_PRIVATE_NETWORKS=true`)"
        )),
        other => Err(format!("{url} uses `{other}`; only https is allowed")),
    }
}

/// Resolve `raw`, check every address, and build a client pinned to them.
pub async fn pin(raw: &str, allow_private: bool, timeout: Duration) -> Result<Pinned, String> {
    let url = Url::parse(raw).map_err(|e| format!("`{raw}` is not a URL ({e})"))?;
    check_scheme(&url, allow_private)?;
    let host = url
        .host_str()
        .ok_or_else(|| format!("`{raw}` names no host"))?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let port = url
        .port_or_known_default()
        .ok_or_else(|| format!("`{raw}` has no port"))?;
    let addrs: Vec<SocketAddr> = match host.parse::<IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, port)],
        Err(_) => tokio::net::lookup_host((host.as_str(), port))
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
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .resolve_to_addrs(&host, &addrs)
        .build()
        .map_err(|e| format!("building the HTTP client failed: {e}"))?;
    Ok(Pinned { url, client })
}

/// The body of `resp` through `capped_read::read_capped`, the errors worded
/// for the agent's owner. `what` names the body (`the agent card`).
pub async fn read_capped(
    resp: reqwest::Response,
    max: usize,
    what: &str,
) -> Result<Vec<u8>, String> {
    capped_read::read_capped(resp, max as u64)
        .await
        .map_err(|e| match e {
            CappedReadError::TooLarge { .. } => format!("{what} is larger than {} KiB", max / 1024),
            CappedReadError::Transport(e) => format!("reading {what} failed: {e}"),
        })
}

/// The shape a card URL needs before anything is fetched: an absolute
/// `https` URL (or `http` to a loopback host, for a local test peer) with a
/// host and no credentials in it.
pub fn check_card_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|e| format!("it is not a URL ({e})"))?;
    let loopback = is_loopback_host(&url.host().ok_or("it names no host")?);
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        _ => return Err("it must be an https URL".into()),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(
            "it carries credentials; put them in the route's `auth` instead, where they are \
             sealed"
                .into(),
        );
    }
    if url.fragment().is_some() {
        return Err("it has a fragment (`#…`); remove it".into());
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

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
        ] {
            let refused = check_ip(ip(addr), false).unwrap_err();
            assert!(
                refused.contains("AIPLANE_A2A_ALLOW_PRIVATE_NETWORKS"),
                "{refused}"
            );
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
    async fn plain_http_and_private_literals_are_refused_before_connecting() {
        let t = Duration::from_secs(1);
        let http = pin("http://93.184.216.34/a2a", false, t)
            .await
            .err()
            .unwrap();
        assert!(http.contains("https"), "{http}");
        let lo = pin("https://127.0.0.1:9/a2a", false, t)
            .await
            .err()
            .unwrap();
        assert!(lo.contains("loopback"), "{lo}");
        let meta = pin("http://169.254.169.254/latest", true, t)
            .await
            .err()
            .unwrap();
        assert!(meta.contains("link-local"), "{meta}");
        assert!(pin("http://127.0.0.1:9/a2a", true, t).await.is_ok());
        assert!(pin("ftp://example.com/x", true, t).await.is_err());
    }

    async fn get(url: &str) -> reqwest::Response {
        let pinned = pin(url, true, Duration::from_secs(30)).await.unwrap();
        pinned.client.get(pinned.url).send().await.unwrap()
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
        let why = read_capped(get(&server.uri()).await, 1024, "the answer")
            .await
            .unwrap_err();
        assert!(why.contains("the answer is larger than 1 KiB"), "{why}");
        let body = read_capped(get(&server.uri()).await, 2048, "the answer")
            .await
            .unwrap();
        assert_eq!(body.len(), 2048);
    }

    #[test]
    fn a_card_url_is_https_without_credentials() {
        assert!(check_card_url("https://partner.example.com/.well-known/agent-card.json").is_ok());
        assert!(check_card_url("http://127.0.0.1:4000/.well-known/agent-card.json").is_ok());
        assert!(check_card_url("http://localhost:4000/card").is_ok());
        assert!(check_card_url("http://partner.example.com/card").is_err());
        assert!(check_card_url("https://user:pw@partner.example.com/card").is_err());
        assert!(check_card_url("https://partner.example.com/card#x").is_err());
        assert!(check_card_url("not a url").is_err());
    }
}
