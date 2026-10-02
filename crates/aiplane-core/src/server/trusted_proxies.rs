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

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TrustedProxyError {
    #[error(
        "`{entry}` in the trusted-proxy list is not an IP address or CIDR network \
         (e.g. `10.0.0.0/8`, `192.0.2.7`, `fd00::/8`): {reason}. Fix `$AIPLANE_TRUSTED_PROXIES` — \
         refusing to start rather than guess which proxies to believe"
    )]
    Invalid { entry: String, reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Network {
    addr: IpAddr,
    prefix: u8,
}

impl Network {
    fn parse(entry: &str) -> Result<Self, TrustedProxyError> {
        let invalid = |reason: &str| TrustedProxyError::Invalid {
            entry: entry.to_string(),
            reason: reason.to_string(),
        };
        let (host, prefix) = match entry.split_once('/') {
            Some((host, prefix)) => (
                host,
                Some(
                    prefix
                        .parse::<u8>()
                        .map_err(|_| invalid("the prefix length is not a number"))?,
                ),
            ),
            None => (entry, None),
        };
        let written: IpAddr = host
            .parse()
            .map_err(|_| invalid("the address does not parse"))?;
        let addr = written.to_canonical();
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            None => max,
            // `::ffff:10.0.0.0/104` names an IPv4 network; peers are matched
            // in their IPv4 form, so the network must be too.
            Some(p) if addr.is_ipv4() && written.is_ipv6() => p
                .checked_sub(96)
                .ok_or_else(|| invalid("an IPv4-mapped network needs a prefix of 96 or more"))?,
            Some(p) => p,
        };
        if prefix > max {
            return Err(invalid(&format!("the prefix length exceeds {max}")));
        }
        Ok(Self { addr, prefix })
    }

    fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip.to_canonical()) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                network_bits(u32::from(net).into(), 32, self.prefix)
                    == network_bits(u32::from(ip).into(), 32, self.prefix)
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                network_bits(net.into(), 128, self.prefix)
                    == network_bits(ip.into(), 128, self.prefix)
            }
            _ => false,
        }
    }
}

fn network_bits(bits: u128, width: u32, prefix: u8) -> u128 {
    if prefix == 0 {
        0
    } else {
        bits >> (width - u32::from(prefix))
    }
}

/// The set of networks whose forwarded headers are believed. Empty by default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustedProxies(Vec<Network>);

impl TrustedProxies {
    /// Parse a comma- or whitespace-separated list of addresses and CIDR
    /// networks. Empty entries are skipped so a trailing comma is harmless.
    pub fn parse(list: &str) -> Result<Self, TrustedProxyError> {
        list.split(|c: char| c == ',' || c.is_whitespace())
            .filter(|e| !e.is_empty())
            .map(Network::parse)
            .collect::<Result<Vec<_>, _>>()
            .map(Self)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn trusts(&self, ip: IpAddr) -> bool {
        self.0.iter().any(|n| n.contains(ip))
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
    fn an_ipv4_mapped_network_is_read_as_ipv4() {
        let p = proxies("::ffff:10.0.0.0/104");
        assert!(p.trusts(ip("10.200.0.1")));
        assert!(!p.trusts(ip("11.0.0.1")));
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
    fn cidr_boundaries_are_exact() {
        let p = proxies("192.0.2.0/25");
        assert!(p.trusts(ip("192.0.2.127")));
        assert!(!p.trusts(ip("192.0.2.128")));
        assert!(proxies("0.0.0.0/0").trusts(ip("8.8.8.8")));
        assert!(!proxies("0.0.0.0/0").trusts(ip("::1")));
        assert!(proxies("192.0.2.7").trusts(ip("192.0.2.7")));
        assert!(!proxies("192.0.2.7").trusts(ip("192.0.2.8")));
    }

    #[test]
    fn list_parsing_tolerates_separators_and_empties() {
        assert!(proxies("").is_empty());
        assert!(proxies(" , ").is_empty());
        let p = proxies("10.0.0.0/8,\n172.16.0.0/12 192.0.2.1,");
        assert!(p.trusts(ip("172.31.0.1")) && p.trusts(ip("192.0.2.1")));
    }

    #[test]
    fn a_bad_entry_is_refused_with_the_entry_named() {
        for bad in [
            "10.0.0.0/33",
            "fd00::/129",
            "nope",
            "10.0.0.0/x",
            "10.0.0.0/",
            "::ffff:10.0.0.0/64",
        ] {
            let err = TrustedProxies::parse(&format!("10.0.0.1, {bad}")).unwrap_err();
            assert!(err.to_string().contains(bad), "{bad}: {err}");
        }
    }
}
