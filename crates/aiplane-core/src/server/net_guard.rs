// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The one place an IP address is sorted into the network it lives in.
//!
//! Every outbound guard (A2A routes, Web Push endpoints, MCP OAuth, the
//! WebDAV RAG source) and the GeoIP lookup ask [`classify`] and apply their
//! own policy to the answer: what each caller refuses differs on purpose,
//! what an address *is* does not. An IPv4-mapped IPv6 address
//! (`::ffff:a.b.c.d`) is classified as the IPv4 address it carries, so a
//! guard cannot be walked around by spelling a private address in IPv6.

use std::net::IpAddr;

/// The network an address belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpClass {
    Public,
    /// `0.0.0.0`, `::`.
    Unspecified,
    /// `127.0.0.0/8`, `::1`.
    Loopback,
    /// RFC 1918 and IPv6 unique local (`fc00::/7`).
    Private,
    /// Carrier-grade NAT, `100.64.0.0/10`.
    Cgnat,
    /// `169.254.0.0/16` (the cloud metadata address among them), `fe80::/10`.
    LinkLocal,
    /// `255.255.255.255`.
    Broadcast,
    /// `224.0.0.0/4`, `ff00::/8`.
    Multicast,
}

impl IpClass {
    pub fn is_public(self) -> bool {
        self == IpClass::Public
    }

    /// The class as a phrase that completes "`<ip>` is …".
    pub fn describe(self) -> &'static str {
        match self {
            IpClass::Public => "a public address",
            IpClass::Unspecified => "an unspecified address",
            IpClass::Loopback => "a loopback address",
            IpClass::Private => "a private address",
            IpClass::Cgnat => "a carrier-grade NAT address",
            IpClass::LinkLocal => "a link-local address (where cloud metadata lives)",
            IpClass::Broadcast => "a broadcast address",
            IpClass::Multicast => "a multicast address",
        }
    }
}

pub fn classify(ip: IpAddr) -> IpClass {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            if v4.is_unspecified() {
                IpClass::Unspecified
            } else if v4.is_loopback() {
                IpClass::Loopback
            } else if v4.is_private() {
                IpClass::Private
            } else if a == 100 && (64..128).contains(&b) {
                IpClass::Cgnat
            } else if v4.is_link_local() {
                IpClass::LinkLocal
            } else if v4.is_broadcast() {
                IpClass::Broadcast
            } else if v4.is_multicast() {
                IpClass::Multicast
            } else {
                IpClass::Public
            }
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return classify(IpAddr::V4(v4));
            }
            // `Ipv6Addr::is_unique_local` / `is_unicast_link_local` are not stable yet.
            let first = v6.segments()[0];
            if v6.is_unspecified() {
                IpClass::Unspecified
            } else if v6.is_loopback() {
                IpClass::Loopback
            } else if (first & 0xfe00) == 0xfc00 {
                IpClass::Private
            } else if (first & 0xffc0) == 0xfe80 {
                IpClass::LinkLocal
            } else if v6.is_multicast() {
                IpClass::Multicast
            } else {
                IpClass::Public
            }
        }
    }
}

/// The class of a URL host that is an address literal; `None` for a name.
pub fn classify_host(host: &url::Host<&str>) -> Option<IpClass> {
    match host {
        url::Host::Domain(_) => None,
        url::Host::Ipv4(v4) => Some(classify(IpAddr::V4(*v4))),
        url::Host::Ipv6(v6) => Some(classify(IpAddr::V6(*v6))),
    }
}

/// `localhost` or a loopback literal: the hosts where plain `http` is
/// tolerated for a local peer.
pub fn is_loopback_host(host: &url::Host<&str>) -> bool {
    match host {
        url::Host::Domain(d) => d.eq_ignore_ascii_case("localhost"),
        _ => classify_host(host) == Some(IpClass::Loopback),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(s: &str) -> IpClass {
        classify(s.parse().unwrap())
    }

    fn on<T>(raw: &str, f: impl Fn(&url::Host<&str>) -> T) -> T {
        f(&url::Url::parse(raw).unwrap().host().unwrap())
    }

    #[test]
    fn every_class_is_recognised_in_both_families() {
        for (addr, want) in [
            ("93.184.216.34", IpClass::Public),
            ("100.128.0.1", IpClass::Public),
            ("2606:4700::1111", IpClass::Public),
            ("0.0.0.0", IpClass::Unspecified),
            ("::", IpClass::Unspecified),
            ("127.0.0.1", IpClass::Loopback),
            ("127.255.0.9", IpClass::Loopback),
            ("::1", IpClass::Loopback),
            ("10.1.2.3", IpClass::Private),
            ("172.16.0.1", IpClass::Private),
            ("192.168.1.1", IpClass::Private),
            ("fd00::1", IpClass::Private),
            ("fc00::1", IpClass::Private),
            ("100.64.0.1", IpClass::Cgnat),
            ("100.127.255.254", IpClass::Cgnat),
            ("169.254.169.254", IpClass::LinkLocal),
            ("fe80::1", IpClass::LinkLocal),
            ("febf::1", IpClass::LinkLocal),
            ("255.255.255.255", IpClass::Broadcast),
            ("224.0.0.1", IpClass::Multicast),
            ("ff02::1", IpClass::Multicast),
        ] {
            assert_eq!(class(addr), want, "{addr}");
        }
    }

    #[test]
    fn an_ipv4_mapped_address_is_classified_as_the_ipv4_it_carries() {
        assert_eq!(class("::ffff:10.0.0.1"), IpClass::Private);
        assert_eq!(class("::ffff:127.0.0.1"), IpClass::Loopback);
        assert_eq!(class("::ffff:169.254.169.254"), IpClass::LinkLocal);
        assert_eq!(class("::ffff:100.64.0.1"), IpClass::Cgnat);
        assert_eq!(class("::ffff:8.8.8.8"), IpClass::Public);
    }

    #[test]
    fn hosts_are_classified_only_when_they_are_literals() {
        assert_eq!(
            on("https://[fe80::1]/", classify_host),
            Some(IpClass::LinkLocal)
        );
        assert_eq!(on("https://example.com/", classify_host), None);
        assert!(on("http://LOCALHOST:4000/", is_loopback_host));
        assert!(on("http://[::1]:4000/", is_loopback_host));
        assert!(on("http://127.0.0.2/", is_loopback_host));
        assert!(!on("http://10.0.0.1/", is_loopback_host));
        assert!(!on("http://example.com/", is_loopback_host));
    }
}
