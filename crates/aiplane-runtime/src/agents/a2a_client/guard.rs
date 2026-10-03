// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The shape an A2A card URL needs before anything is fetched. Where the
//! card, the endpoint and the token URL may then be reached is
//! `aiplane_core::server::outbound_guard`'s call (`Policy::agent`), shared
//! with every other destination someone other than the operator chooses.

use aiplane_core::server::net_guard::is_loopback_host;
use reqwest::Url;

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
