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
//! Not reused from `mcp_oauth::validate_outbound_url`: that guard allows
//! private ranges on purpose (an admin curates the MCP catalog) and checks
//! literal addresses only, while a route's target is chosen by an agent's
//! owner and must not reach the gateway's own network.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use reqwest::Url;

/// A host that passed the guard, and the client pinned to its addresses.
pub struct Pinned {
    pub url: Url,
    pub client: reqwest::Client,
}

/// Why an address may never be connected to, whatever the configuration.
fn always_refused(ip: IpAddr) -> Option<&'static str> {
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_unspecified() {
                Some("an unspecified address")
            } else if v4.is_link_local() {
                Some("a link-local address (where cloud metadata lives)")
            } else if v4.is_broadcast() {
                Some("a broadcast address")
            } else if v4.is_multicast() {
                Some("a multicast address")
            } else {
                None
            }
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return always_refused(IpAddr::V4(v4));
            }
            if v6.is_unspecified() {
                Some("an unspecified address")
            } else if (v6.segments()[0] & 0xffc0) == 0xfe80 {
                Some("a link-local address")
            } else if v6.is_multicast() {
                Some("a multicast address")
            } else {
                None
            }
        }
    }
}

/// Why an address is inside a network the gateway itself sits in, if it is.
fn private(ip: IpAddr) -> Option<&'static str> {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            if v4.is_loopback() {
                Some("a loopback address")
            } else if v4.is_private() {
                Some("a private address")
            } else if a == 100 && (64..128).contains(&b) {
                Some("a carrier-grade NAT address")
            } else {
                None
            }
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return private(IpAddr::V4(v4));
            }
            if v6.is_loopback() {
                Some("a loopback address")
            } else if (v6.segments()[0] & 0xfe00) == 0xfc00 {
                Some("a unique local (private) address")
            } else {
                None
            }
        }
    }
}

/// Whether `ip` may be connected to, and why not.
pub fn check_ip(ip: IpAddr, allow_private: bool) -> Result<(), String> {
    if let Some(why) = always_refused(ip) {
        return Err(format!("{ip} is {why}, which is never reached"));
    }
    if !allow_private && let Some(why) = private(ip) {
        return Err(format!(
            "{ip} is {why}; an agent reaches public hosts only unless the operator sets \
             `$AIPLANE_A2A_ALLOW_PRIVATE_NETWORKS=true`"
        ));
    }
    Ok(())
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

/// The body of `resp`, refused once it is longer than `max` bytes: a
/// declared `Content-Length` over the cap is refused before anything is read,
/// and a body without one is read chunk by chunk and dropped the moment the
/// running total passes the cap, so a hostile peer cannot make the gateway
/// buffer more than `max`. `what` names the body in the error (`the agent
/// card`).
pub async fn read_capped(
    mut resp: reqwest::Response,
    max: usize,
    what: &str,
) -> Result<Vec<u8>, String> {
    let too_large = || format!("{what} is larger than {} KiB", max / 1024);
    if resp.content_length().is_some_and(|len| len > max as u64) {
        return Err(too_large());
    }
    let mut body = Vec::with_capacity(resp.content_length().unwrap_or(0) as usize);
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("reading {what} failed: {e}"))?
    {
        if body.len() + chunk.len() > max {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// The shape a card URL needs before anything is fetched: an absolute
/// `https` URL (or `http` to a loopback host, for a local test peer) with a
/// host and no credentials in it.
pub fn check_card_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|e| format!("it is not a URL ({e})"))?;
    let host = url.host_str().ok_or("it names no host")?;
    let loopback = host == "localhost"
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|ip| {
                ip == IpAddr::V4(Ipv4Addr::LOCALHOST) || ip == IpAddr::V6(Ipv6Addr::LOCALHOST)
            });
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

    /// A peer that answers with a chunked body and no `Content-Length`, one
    /// 64 KiB chunk after another until the client hangs up. A raw socket
    /// because wiremock always sends a length.
    async fn endless_chunked_peer() -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let head = "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                        transfer-encoding: chunked\r\n\r\n";
            if sock.write_all(head.as_bytes()).await.is_err() {
                return;
            }
            let chunk = format!("10000\r\n{}\r\n", " ".repeat(0x10000));
            while sock.write_all(chunk.as_bytes()).await.is_ok() {}
        });
        format!("http://{addr}/")
    }

    async fn get(url: &str) -> reqwest::Response {
        let pinned = pin(url, true, Duration::from_secs(30)).await.unwrap();
        pinned.client.get(pinned.url).send().await.unwrap()
    }

    #[tokio::test]
    async fn a_body_without_a_length_is_cut_off_at_the_cap() {
        let url = endless_chunked_peer().await;
        let read = tokio::time::timeout(
            Duration::from_secs(10),
            read_capped(get(&url).await, 256 * 1024, "the agent card"),
        )
        .await
        .expect("reading stopped at the cap instead of draining the stream");
        let why = read.unwrap_err();
        assert!(
            why.contains("the agent card is larger than 256 KiB"),
            "{why}"
        );
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
