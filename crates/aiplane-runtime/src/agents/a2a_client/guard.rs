// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The shape an A2A card URL needs before anything is fetched. Where the
//! card, the endpoint and the token URL may then be reached is
//! `aiplane_core::server::outbound_guard`'s call (`Policy::agent`), shared
//! with every other destination someone other than the operator chooses.
//! The card URL is checked against that same policy when it is granted and
//! when a spec names it, so a URL the run would refuse is refused there
//! already.

use aiplane_core::server::outbound_guard::{self, Policy};
use reqwest::Url;

/// The shape a card URL needs before anything is fetched: what
/// `outbound_guard::check_url` takes under [`Policy::agent`] — `https`, plain
/// `http` and private hosts only where the operator allows private networks
/// — without credentials or a fragment.
pub fn check_card_url(raw: &str, allow_private: bool) -> Result<Url, String> {
    if let Ok(url) = Url::parse(raw)
        && (!url.username().is_empty() || url.password().is_some())
    {
        return Err(
            "it carries credentials; put them in the route's `auth` instead, where they are \
             sealed"
                .into(),
        );
    }
    let url = outbound_guard::check_url(raw, Policy::agent(allow_private))?;
    if url.fragment().is_some() {
        return Err("it has a fragment (`#…`); remove it".into());
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_url_is_what_the_run_may_reach_without_credentials() {
        assert!(
            check_card_url(
                "https://partner.example.com/.well-known/agent-card.json",
                false
            )
            .is_ok()
        );
        assert!(
            check_card_url("http://127.0.0.1:4000/.well-known/agent-card.json", false).is_err()
        );
        assert!(check_card_url("https://localhost:4000/card", false).is_err());
        assert!(check_card_url("http://127.0.0.1:4000/.well-known/agent-card.json", true).is_ok());
        assert!(check_card_url("http://localhost:4000/card", true).is_ok());
        assert!(check_card_url("http://partner.example.com/card", false).is_err());
        let creds = check_card_url("https://user:pw@partner.example.com/card", true).unwrap_err();
        assert!(creds.contains("`auth`"), "{creds}");
        assert!(check_card_url("https://partner.example.com/card#x", false).is_err());
        assert!(check_card_url("not a url", false).is_err());
    }
}
