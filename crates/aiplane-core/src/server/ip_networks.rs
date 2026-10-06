// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! A list of IP addresses and CIDR networks, and whether an address falls in
//! one of them.
//!
//! The one parser for operator-written network lists: the trusted reverse
//! proxies (`$AIPLANE_TRUSTED_PROXIES`) and the addresses allowed to scrape
//! `/metrics` both read their entries through it, so `10.0.0.0/8`,
//! `192.0.2.7`, `fd00::/8` and an IPv4-mapped `::ffff:10.0.0.0/104` mean the
//! same thing everywhere.

use std::net::IpAddr;

use thiserror::Error;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error(
    "`{entry}` is not an IP address or CIDR network (e.g. `10.0.0.0/8`, `192.0.2.7`, \
     `fd00::/8`): {fault}"
)]
pub struct InvalidNetwork {
    pub entry: String,
    pub fault: NetworkFault,
}

/// What is wrong with an entry. A type rather than a sentence, so the
/// settings editor can say it in the operator's language.
#[derive(Debug, Clone, Copy, Error, PartialEq, Eq)]
pub enum NetworkFault {
    #[error("the prefix length is not a number")]
    PrefixNotNumber,
    #[error("the address does not parse")]
    AddressDoesNotParse,
    #[error("an IPv4-mapped network needs a prefix of 96 or more")]
    MappedPrefixTooShort,
    #[error("the prefix length exceeds {max}")]
    PrefixTooLong { max: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Network {
    addr: IpAddr,
    prefix: u8,
}

impl Network {
    fn parse(entry: &str) -> Result<Self, InvalidNetwork> {
        let invalid = |fault: NetworkFault| InvalidNetwork {
            entry: entry.to_string(),
            fault,
        };
        let (host, prefix) = match entry.split_once('/') {
            Some((host, prefix)) => (
                host,
                Some(
                    prefix
                        .parse::<u8>()
                        .map_err(|_| invalid(NetworkFault::PrefixNotNumber))?,
                ),
            ),
            None => (entry, None),
        };
        let written: IpAddr = host
            .parse()
            .map_err(|_| invalid(NetworkFault::AddressDoesNotParse))?;
        let addr = written.to_canonical();
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            None => max,
            // `::ffff:10.0.0.0/104` names an IPv4 network; addresses are
            // matched in their IPv4 form, so the network must be too.
            Some(p) if addr.is_ipv4() && written.is_ipv6() => p
                .checked_sub(96)
                .ok_or_else(|| invalid(NetworkFault::MappedPrefixTooShort))?,
            Some(p) => p,
        };
        if prefix > max {
            return Err(invalid(NetworkFault::PrefixTooLong { max }));
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

/// The entries of a comma- or whitespace-separated list, empty ones skipped so
/// a trailing comma is harmless.
pub fn split_entries(list: &str) -> impl Iterator<Item = &str> {
    list.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|e| !e.is_empty())
}

/// A set of IP networks. Empty by default, and an empty set contains nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IpNetworks(Vec<Network>);

impl IpNetworks {
    /// Parse a comma- or whitespace-separated list (see [`split_entries`]).
    pub fn parse(list: &str) -> Result<Self, InvalidNetwork> {
        Self::from_entries(split_entries(list))
    }

    /// Parse one entry per item. The first entry that is not an address or a
    /// network fails the whole list, naming it: a list with an entry silently
    /// dropped would allow or trust something other than what was written.
    pub fn from_entries<'a>(
        entries: impl IntoIterator<Item = &'a str>,
    ) -> Result<Self, InvalidNetwork> {
        entries
            .into_iter()
            .map(|entry| Network::parse(entry.trim()))
            .collect::<Result<Vec<_>, _>>()
            .map(Self)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        self.0.iter().any(|n| n.contains(ip))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn nets(list: &str) -> IpNetworks {
        IpNetworks::parse(list).unwrap()
    }

    #[test]
    fn cidr_boundaries_are_exact() {
        let n = nets("192.0.2.0/25");
        assert!(n.contains(ip("192.0.2.127")));
        assert!(!n.contains(ip("192.0.2.128")));
        assert!(nets("0.0.0.0/0").contains(ip("8.8.8.8")));
        assert!(!nets("0.0.0.0/0").contains(ip("::1")));
        assert!(nets("192.0.2.7").contains(ip("192.0.2.7")));
        assert!(!nets("192.0.2.7").contains(ip("192.0.2.8")));
    }

    #[test]
    fn ipv6_networks_and_addresses() {
        let n = nets("2001:db8::/32, ::1");
        assert!(n.contains(ip("2001:db8:ffff::9")));
        assert!(!n.contains(ip("2001:db9::1")));
        assert!(n.contains(ip("::1")));
        assert!(!n.contains(ip("127.0.0.1")));
    }

    #[test]
    fn an_ipv4_mapped_network_is_read_as_ipv4() {
        let n = nets("::ffff:10.0.0.0/104");
        assert!(n.contains(ip("10.200.0.1")));
        assert!(!n.contains(ip("11.0.0.1")));
    }

    #[test]
    fn an_ipv4_mapped_address_matches_its_ipv4_network() {
        assert!(nets("10.0.0.0/8").contains(ip("::ffff:10.0.0.2")));
    }

    #[test]
    fn an_empty_set_contains_nothing() {
        assert!(IpNetworks::default().is_empty());
        assert!(!IpNetworks::default().contains(ip("127.0.0.1")));
    }

    #[test]
    fn list_parsing_tolerates_separators_and_empties() {
        assert!(nets("").is_empty());
        assert!(nets(" , ").is_empty());
        let n = nets("10.0.0.0/8,\n172.16.0.0/12 192.0.2.1,");
        assert!(n.contains(ip("172.31.0.1")) && n.contains(ip("192.0.2.1")));
    }

    #[test]
    fn entries_are_parsed_one_per_item() {
        let n = IpNetworks::from_entries([" 10.0.0.0/8 ", "2001:db8::/32"]).unwrap();
        assert!(n.contains(ip("10.1.1.1")) && n.contains(ip("2001:db8::1")));
    }

    #[test]
    fn a_bad_entry_is_refused_with_the_entry_named() {
        for (bad, fault) in [
            ("10.0.0.0/33", NetworkFault::PrefixTooLong { max: 32 }),
            ("fd00::/129", NetworkFault::PrefixTooLong { max: 128 }),
            ("nope", NetworkFault::AddressDoesNotParse),
            ("10.0.0.0/x", NetworkFault::PrefixNotNumber),
            ("10.0.0.0/", NetworkFault::PrefixNotNumber),
            ("::ffff:10.0.0.0/64", NetworkFault::MappedPrefixTooShort),
        ] {
            let err = IpNetworks::parse(&format!("10.0.0.1, {bad}")).unwrap_err();
            assert_eq!(err.entry, bad);
            assert_eq!(err.fault, fault, "{bad}");
            assert!(err.to_string().contains(bad), "{bad}: {err}");
        }
    }
}
