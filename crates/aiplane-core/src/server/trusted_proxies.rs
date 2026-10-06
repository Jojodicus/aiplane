// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Which TCP peers may tell us who the real client is.
//!
//! `X-Forwarded-For` and `X-Real-IP` are plain request headers: any client can
//! send them. They are only evidence about the client when the connection came
//! from a reverse proxy that overwrites or appends to them. So the client IP is
//! decided here, once, for every consumer (GeoIP, request context, the public
//! agent's per-IP rate limit): the TCP peer is the answer unless it is a
//! configured trusted proxy, in which case the rightmost `X-Forwarded-For` hop
//! that is *not* itself a trusted proxy is.
//!
//! Default is no trusted proxy: a gateway exposed directly never believes a
//! header.

use std::net::{IpAddr, Ipv6Addr, SocketAddr};

use rama::http::HeaderMap;
use thiserror::Error;

use crate::server::ip_networks::{InvalidNetwork, IpNetworks};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TrustedProxyError {
    #[error(
        "in the trusted-proxy list, {0}. Fix `$AIPLANE_TRUSTED_PROXIES` — refusing to start \
         rather than guess which proxies to believe"
    )]
    Invalid(#[from] InvalidNetwork),
}

/// The set of networks whose forwarded headers are believed. Empty by default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustedProxies(IpNetworks);

impl TrustedProxies {
    /// Parse a comma- or whitespace-separated list of addresses and CIDR
    /// networks. Empty entries are skipped so a trailing comma is harmless.
    pub fn parse(list: &str) -> Result<Self, TrustedProxyError> {
        Ok(Self(IpNetworks::parse(list)?))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn trusts(&self, ip: IpAddr) -> bool {
        self.0.contains(ip)
    }

    /// The client behind a request that arrived from `peer` with `headers`.
    ///
    /// `peer` is the TCP peer (`None` only when the request never crossed a
    /// socket). Headers are consulted only if the peer is trusted. A forwarded
    /// chain that cannot be read to its end falls back to the peer — the proxy
    /// — which merges clients into one bucket rather than letting a forged
    /// entry choose its own.
    pub fn client_ip(&self, peer: Option<IpAddr>, headers: &HeaderMap) -> Option<IpAddr> {
        let peer = peer?.to_canonical();
        if !self.trusts(peer) {
            return Some(peer);
        }
        let forwarded: Vec<&str> = headers
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .collect();
        if !forwarded.is_empty() {
            return Some(self.rightmost_untrusted(&forwarded).unwrap_or(peer));
        }
        let real = headers
            .get("x-real-ip")
            .and_then(|v| v.to_str().ok())
            .and_then(parse_hop);
        Some(real.unwrap_or(peer))
    }

    fn rightmost_untrusted(&self, hops: &[&str]) -> Option<IpAddr> {
        let mut last = None;
        for hop in hops.iter().rev() {
            let ip = parse_hop(hop)?;
            if !self.trusts(ip) {
                return Some(ip);
            }
            last = Some(ip);
        }
        last
    }
}

/// A hop as proxies write it: a bare address, `ip:port`, `[v6]` or `[v6]:port`.
fn parse_hop(raw: &str) -> Option<IpAddr> {
    let raw = raw.trim();
    raw.parse::<IpAddr>()
        .or_else(|_| raw.parse::<SocketAddr>().map(|s| s.ip()))
        .or_else(|_| {
            raw.strip_prefix('[')
                .and_then(|r| r.strip_suffix(']'))
                .unwrap_or(raw)
                .parse::<Ipv6Addr>()
                .map(IpAddr::V6)
        })
        .ok()
        .map(|ip| ip.to_canonical())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(
                rama::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        h
    }

    fn proxies(list: &str) -> TrustedProxies {
        TrustedProxies::parse(list).unwrap()
    }

    fn client(p: &TrustedProxies, peer: &str, h: &[(&str, &str)]) -> Option<IpAddr> {
        p.client_ip(Some(ip(peer)), &headers(h))
    }

    #[test]
    fn with_no_trusted_proxy_a_spoofed_header_is_ignored() {
        let p = TrustedProxies::default();
        let h = [("x-forwarded-for", "1.2.3.4"), ("x-real-ip", "5.6.7.8")];
        assert_eq!(client(&p, "198.51.100.9", &h), Some(ip("198.51.100.9")));
    }

    #[test]
    fn an_untrusted_peer_cannot_borrow_a_trusted_proxys_authority() {
        let p = proxies("10.0.0.0/8");
        let h = [("x-forwarded-for", "1.2.3.4, 10.0.0.2")];
        assert_eq!(client(&p, "198.51.100.9", &h), Some(ip("198.51.100.9")));
    }

    #[test]
    fn a_trusted_peer_vouches_for_the_forwarded_client() {
        let p = proxies("10.0.0.0/8");
        let h = [("x-forwarded-for", "203.0.113.5")];
        assert_eq!(client(&p, "10.1.2.3", &h), Some(ip("203.0.113.5")));
    }

    #[test]
    fn a_chain_through_two_trusted_proxies_resolves_to_the_real_client() {
        let p = proxies("10.0.0.0/8, 192.168.1.1");
        let h = [("x-forwarded-for", "203.0.113.5, 192.168.1.1")];
        assert_eq!(client(&p, "10.0.0.2", &h), Some(ip("203.0.113.5")));
    }

    #[test]
    fn a_forged_leftmost_entry_does_not_beat_the_real_client() {
        let p = proxies("10.0.0.0/8");
        // The attacker prepended 1.2.3.4; the proxy appended the real peer.
        let h = [("x-forwarded-for", "1.2.3.4, 203.0.113.5")];
        assert_eq!(client(&p, "10.0.0.2", &h), Some(ip("203.0.113.5")));
    }

    #[test]
    fn repeated_header_lines_are_one_chain() {
        let p = proxies("10.0.0.0/8");
        let h = [
            ("x-forwarded-for", "1.2.3.4"),
            ("x-forwarded-for", "203.0.113.5, 10.0.0.9"),
        ];
        assert_eq!(client(&p, "10.0.0.2", &h), Some(ip("203.0.113.5")));
    }

    #[test]
    fn a_chain_made_only_of_trusted_proxies_names_its_origin() {
        let p = proxies("10.0.0.0/8");
        let h = [("x-forwarded-for", "10.9.9.9, 10.0.0.3")];
        assert_eq!(client(&p, "10.0.0.2", &h), Some(ip("10.9.9.9")));
    }

    #[test]
    fn real_ip_is_honoured_from_a_trusted_peer_only() {
        let p = proxies("10.0.0.0/8");
        let h = [("x-real-ip", "203.0.113.5")];
        assert_eq!(client(&p, "10.0.0.2", &h), Some(ip("203.0.113.5")));
        assert_eq!(client(&p, "198.51.100.9", &h), Some(ip("198.51.100.9")));
    }

    #[test]
    fn forwarded_for_wins_over_real_ip() {
        let p = proxies("10.0.0.0/8");
        let h = [("x-forwarded-for", "203.0.113.5"), ("x-real-ip", "1.1.1.1")];
        assert_eq!(client(&p, "10.0.0.2", &h), Some(ip("203.0.113.5")));
    }

    #[test]
    fn malformed_headers_fall_back_to_the_peer() {
        let p = proxies("10.0.0.0/8");
        for bad in [
            "garbage",
            "",
            "203.0.113.5, not-an-ip",
            "203.0.113.5,,10.0.0.1",
            "999.1.1.1",
        ] {
            let h = [("x-forwarded-for", bad)];
            assert_eq!(client(&p, "10.0.0.2", &h), Some(ip("10.0.0.2")), "{bad:?}");
        }
        let h = [("x-real-ip", "nope")];
        assert_eq!(client(&p, "10.0.0.2", &h), Some(ip("10.0.0.2")));
    }

    #[test]
    fn a_non_utf8_header_falls_back_to_the_peer() {
        let p = proxies("10.0.0.0/8");
        let mut h = HeaderMap::new();
        h.insert(
            "x-forwarded-for",
            rama::http::HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap(),
        );
        assert_eq!(p.client_ip(Some(ip("10.0.0.2")), &h), Some(ip("10.0.0.2")));
    }

    #[test]
    fn no_socket_means_no_client_ip() {
        let p = proxies("10.0.0.0/8");
        let h = headers(&[("x-forwarded-for", "1.2.3.4")]);
        assert_eq!(p.client_ip(None, &h), None);
    }

    #[test]
    fn ipv6_networks_and_clients() {
        let p = proxies("fd00::/8");
        let h = [("x-forwarded-for", "2001:db8::5, fd00::2")];
        assert_eq!(client(&p, "fd00::1", &h), Some(ip("2001:db8::5")));
        assert_eq!(client(&p, "2001:db8::99", &h), Some(ip("2001:db8::99")));
    }

    #[test]
    fn ipv4_mapped_addresses_are_the_same_as_their_ipv4_form() {
        let p = proxies("10.0.0.0/8");
        let h = [("x-forwarded-for", "203.0.113.5")];
        assert_eq!(client(&p, "::ffff:10.0.0.2", &h), Some(ip("203.0.113.5")));
        let h = [("x-forwarded-for", "::ffff:203.0.113.5")];
        assert_eq!(client(&p, "10.0.0.2", &h), Some(ip("203.0.113.5")));
        assert_eq!(
            client(&TrustedProxies::default(), "::ffff:198.51.100.9", &[]),
            Some(ip("198.51.100.9"))
        );
    }

    #[test]
    fn hops_may_carry_ports_and_brackets() {
        let p = proxies("10.0.0.0/8");
        for (hop, want) in [
            ("203.0.113.5:4711", "203.0.113.5"),
            ("[2001:db8::5]:4711", "2001:db8::5"),
            ("[2001:db8::5]", "2001:db8::5"),
        ] {
            let h = [("x-forwarded-for", hop)];
            assert_eq!(client(&p, "10.0.0.2", &h), Some(ip(want)), "{hop}");
        }
    }

    #[test]
    fn a_bad_entry_names_itself_and_the_variable_to_fix() {
        let err = TrustedProxies::parse("10.0.0.1, 10.0.0.0/33").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("10.0.0.0/33"), "{message}");
        assert!(message.contains("AIPLANE_TRUSTED_PROXIES"), "{message}");
    }
}
