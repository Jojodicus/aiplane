// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Gateway-token primitives.
//!
//! - Tokens are opaque random strings prefixed `gwk_`, 32 random bytes encoded
//!   hex (so the wire form looks like `gwk_<64 hex chars>`). 256-bit entropy.
//! - In the DB we store **SHA-256 hex** of the bearer string, not the plaintext
//!   and not an argon2id hash. The token is high-entropy random, so SHA-256 is
//!   enough: collisions need ~2^128 work, brute-force needs ~2^256. argon2id's
//!   cost is wasted on a random opaque token, and lookup must be fast for /v1/*.

use crate::server::crypto::{random_hex, sha256_hex};

pub const TOKEN_PREFIX: &str = "gwk_";
/// Webhook trigger secrets share the token construction but carry their own
/// prefix so the two namespaces can't be confused. The `gwh_<64 hex>` string
/// is the credential in a webhook's trigger URL; only its hash is persisted.
pub const WEBHOOK_PREFIX: &str = "gwh_";
/// System-principal tokens (`docs/agents.md` §1). A separate prefix from
/// user tokens so `require_bearer` can route by it: a `gws_` bearer is only
/// ever looked up in `system_tokens`, a `gwk_` one only in `tokens`.
pub const SYSTEM_TOKEN_PREFIX: &str = "gws_";
/// Agent embed keys (`docs/agents.md` §5). Public by design — the key ships
/// in the embedding site's page source — but hashed at rest like the rest.
pub const EMBED_KEY_PREFIX: &str = "gwe_";
/// Visitor tokens: one anonymous visitor conversation, held in the host
/// page's `sessionStorage`. Never accepted anywhere but `/api/v0/embed/*`.
pub const VISITOR_TOKEN_PREFIX: &str = "gwv_";
pub const TOKEN_BYTES: usize = 32;
pub const TOKEN_HEX_LEN: usize = TOKEN_BYTES * 2;

/// Mints a fresh API token (`gwk_…`). Returns `(plaintext, sha256_hex)`. The
/// plaintext is shown to the user exactly once; the hash is what gets persisted.
pub fn mint() -> (String, String) {
    mint_with_prefix(TOKEN_PREFIX)
}

/// Mints a fresh webhook trigger secret (`gwh_…`). Same construction as
/// [`mint`], different prefix. Returns `(plaintext, sha256_hex)`.
pub fn mint_webhook() -> (String, String) {
    mint_with_prefix(WEBHOOK_PREFIX)
}

/// Mints a fresh system-principal token (`gws_…`). Returns
/// `(plaintext, sha256_hex)`.
pub fn mint_system() -> (String, String) {
    mint_with_prefix(SYSTEM_TOKEN_PREFIX)
}

/// Mints a fresh agent embed key (`gwe_…`). Returns `(plaintext, sha256_hex)`.
pub fn mint_embed_key() -> (String, String) {
    mint_with_prefix(EMBED_KEY_PREFIX)
}

/// Validates an embed key (`gwe_…`) and returns its SHA-256 hex.
pub fn hash_embed_key(key: &str) -> Option<String> {
    hash_with_prefix(EMBED_KEY_PREFIX, key)
}

/// Mints a fresh visitor token (`gwv_…`). Returns `(plaintext, sha256_hex)`.
pub fn mint_visitor() -> (String, String) {
    mint_with_prefix(VISITOR_TOKEN_PREFIX)
}

/// Validates a visitor bearer (`gwv_…`) and returns its SHA-256 hex.
pub fn hash_visitor_token(bearer: &str) -> Option<String> {
    hash_with_prefix(VISITOR_TOKEN_PREFIX, bearer)
}

/// Validates a system-principal bearer (`gws_…`) and returns its SHA-256 hex.
pub fn hash_system_bearer(bearer: &str) -> Option<String> {
    hash_with_prefix(SYSTEM_TOKEN_PREFIX, bearer)
}

/// Validates the surface shape of a bearer string and returns its SHA-256 hex
/// for DB lookup. Rejects anything that doesn't look like our format so we
/// never run a hash + DB query on obvious garbage.
pub fn hash_bearer(bearer: &str) -> Option<String> {
    hash_with_prefix(TOKEN_PREFIX, bearer)
}

/// Validates a webhook trigger secret (`gwh_…`) and returns its SHA-256 hex for
/// DB lookup. Same gate as [`hash_bearer`], different prefix.
pub fn hash_webhook_secret(secret: &str) -> Option<String> {
    hash_with_prefix(WEBHOOK_PREFIX, secret)
}

fn mint_with_prefix(prefix: &str) -> (String, String) {
    let plaintext = format!("{prefix}{}", random_hex(TOKEN_BYTES));
    let hash = sha256_hex(plaintext.as_bytes());
    (plaintext, hash)
}

fn hash_with_prefix(prefix: &str, s: &str) -> Option<String> {
    if !s.starts_with(prefix) {
        return None;
    }
    let tail = &s[prefix.len()..];
    if tail.len() != TOKEN_HEX_LEN || !tail.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(sha256_hex(s.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_returns_well_formed_token_and_matching_hash() {
        let (plaintext, hash) = mint();
        assert!(plaintext.starts_with(TOKEN_PREFIX));
        assert_eq!(plaintext.len(), TOKEN_PREFIX.len() + TOKEN_HEX_LEN);
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(hash_bearer(&plaintext).unwrap(), hash);
    }

    #[test]
    fn system_tokens_and_user_tokens_never_validate_as_each_other() {
        let (system, system_hash) = mint_system();
        assert!(system.starts_with(SYSTEM_TOKEN_PREFIX));
        assert_eq!(hash_system_bearer(&system).unwrap(), system_hash);
        assert_eq!(hash_bearer(&system), None);
        let (user, _) = mint();
        assert_eq!(hash_system_bearer(&user), None);
    }

    #[test]
    fn embed_keys_and_visitor_tokens_validate_only_under_their_own_prefix() {
        let (key, key_hash) = mint_embed_key();
        let (visitor, visitor_hash) = mint_visitor();
        assert!(key.starts_with(EMBED_KEY_PREFIX));
        assert!(visitor.starts_with(VISITOR_TOKEN_PREFIX));
        assert_eq!(hash_embed_key(&key).unwrap(), key_hash);
        assert_eq!(hash_visitor_token(&visitor).unwrap(), visitor_hash);
        assert_eq!(hash_visitor_token(&key), None);
        assert_eq!(hash_embed_key(&visitor), None);
        for other in [&key, &visitor] {
            assert_eq!(hash_bearer(other), None);
            assert_eq!(hash_system_bearer(other), None);
        }
        let (user, _) = mint();
        assert_eq!(hash_visitor_token(&user), None);
        assert_eq!(hash_embed_key(&user), None);
    }

    #[test]
    fn mint_is_unique() {
        let (a, _) = mint();
        let (b, _) = mint();
        assert_ne!(a, b);
    }

    #[test]
    fn hash_bearer_rejects_wrong_prefix() {
        assert!(hash_bearer("sk_1234567890abcdef".repeat(8).as_str()).is_none());
        assert!(hash_bearer("").is_none());
    }

    #[test]
    fn hash_bearer_rejects_wrong_length() {
        assert!(hash_bearer("gwk_abc").is_none());
        let long = format!("gwk_{}", "a".repeat(TOKEN_HEX_LEN + 1));
        assert!(hash_bearer(&long).is_none());
    }

    #[test]
    fn hash_bearer_rejects_non_hex() {
        let bad = format!("gwk_{}", "z".repeat(TOKEN_HEX_LEN));
        assert!(hash_bearer(&bad).is_none());
    }

    #[test]
    fn hash_bearer_is_deterministic() {
        let (plaintext, hash) = mint();
        assert_eq!(hash_bearer(&plaintext).unwrap(), hash);
        assert_eq!(hash_bearer(&plaintext).unwrap(), hash);
    }

    #[test]
    fn webhook_secret_round_trips_and_is_namespaced() {
        let (plaintext, hash) = mint_webhook();
        assert!(plaintext.starts_with(WEBHOOK_PREFIX));
        assert_eq!(plaintext.len(), WEBHOOK_PREFIX.len() + TOKEN_HEX_LEN);
        assert_eq!(hash_webhook_secret(&plaintext).unwrap(), hash);
        // The two namespaces must not validate each other's strings.
        assert!(hash_bearer(&plaintext).is_none());
        let (api, _) = mint();
        assert!(hash_webhook_secret(&api).is_none());
    }
}
