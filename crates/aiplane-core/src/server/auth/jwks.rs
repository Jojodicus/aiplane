// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! One cache for JSON Web Key Sets, shared by everything that verifies a
//! token against someone else's keys: the OIDC login (the IdP's `jwks_uri`)
//! and an agent's `host_jwt` verifier (the website's JWKS). Each caller
//! fetches and parses its set its own way — the IdP is the operator's own
//! configuration, a website's address goes through `outbound_guard` — and
//! hands the cache the fetch; the cache decides when to call it.
//!
//! A set is used while it is fresh (`ttl`; `None` keeps it until a token
//! names a key it lacks). A token naming an unknown key refetches the set, so
//! a rotated key is picked up without a restart — but at most once per
//! `min_refetch`, so a token with a made-up `kid` cannot make the gateway
//! fetch someone's keys on every request.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct JwksCache<S> {
    sets: Mutex<HashMap<String, Fetched<S>>>,
    ttl: Option<Duration>,
    min_refetch: Duration,
}

struct Fetched<S> {
    at: Instant,
    set: Arc<S>,
}

impl<S> JwksCache<S> {
    pub fn new(ttl: Option<Duration>, min_refetch: Duration) -> Self {
        Self {
            sets: Mutex::new(HashMap::new()),
            ttl,
            min_refetch,
        }
    }

    /// The key `find` picks from the set at `url`: from the cache while it is
    /// fresh and holds one, else from the set `fetch` returns. `Ok(None)` when
    /// no set holds it, including when the set was fetched too recently to
    /// fetch again.
    pub async fn key<T, E, Fut>(
        &self,
        url: &str,
        find: impl Fn(&S) -> Option<T>,
        fetch: impl FnOnce() -> Fut,
    ) -> Result<Option<T>, E>
    where
        Fut: Future<Output = Result<S, E>>,
    {
        if let Some((set, recent)) = self.cached(url) {
            if let Some(key) = find(&set) {
                return Ok(Some(key));
            }
            if recent {
                return Ok(None);
            }
        }
        let set = Arc::new(fetch().await?);
        self.lock().insert(
            url.to_string(),
            Fetched {
                at: Instant::now(),
                set: set.clone(),
            },
        );
        Ok(find(&set))
    }

    /// The set at `url` as last fetched, fresh or not — for an error message
    /// that names the keys there are.
    pub fn last(&self, url: &str) -> Option<Arc<S>> {
        self.lock().get(url).map(|f| f.set.clone())
    }

    fn cached(&self, url: &str) -> Option<(Arc<S>, bool)> {
        let sets = self.lock();
        let fetched = sets.get(url)?;
        let age = fetched.at.elapsed();
        if self.ttl.is_some_and(|ttl| age >= ttl) {
            return None;
        }
        Some((fetched.set.clone(), age < self.min_refetch))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Fetched<S>>> {
        self.sets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const URL: &str = "https://idp.example/jwks";

    struct Counting {
        fetches: AtomicUsize,
        keys: Mutex<Vec<&'static str>>,
    }

    impl Counting {
        fn new(keys: &[&'static str]) -> Self {
            Self {
                fetches: AtomicUsize::new(0),
                keys: Mutex::new(keys.to_vec()),
            }
        }

        async fn fetch(&self) -> Result<Vec<&'static str>, String> {
            self.fetches.fetch_add(1, Ordering::SeqCst);
            Ok(self.keys.lock().unwrap().clone())
        }

        fn fetches(&self) -> usize {
            self.fetches.load(Ordering::SeqCst)
        }
    }

    fn named(kid: &'static str) -> impl Fn(&Vec<&'static str>) -> Option<&'static str> {
        move |set| set.iter().copied().find(|k| *k == kid)
    }

    #[tokio::test]
    async fn a_known_key_is_served_from_the_cache() {
        let cache = JwksCache::new(None, Duration::from_secs(60));
        let idp = Counting::new(&["k1"]);

        for _ in 0..3 {
            let key = cache.key(URL, named("k1"), || idp.fetch()).await;
            assert_eq!(key, Ok(Some("k1")));
        }
        assert_eq!(idp.fetches(), 1);
    }

    #[tokio::test]
    async fn an_unknown_key_refetches_at_most_once_per_window() {
        let cache = JwksCache::new(None, Duration::from_secs(60));
        let idp = Counting::new(&["k1"]);
        cache.key(URL, named("k1"), || idp.fetch()).await.unwrap();

        for _ in 0..3 {
            let key = cache.key(URL, named("made-up"), || idp.fetch()).await;
            assert_eq!(key, Ok(None));
        }
        assert_eq!(idp.fetches(), 1, "a made-up kid fetches nothing");
    }

    #[tokio::test]
    async fn a_rotated_key_is_picked_up_once_the_window_passed() {
        let cache = JwksCache::new(None, Duration::ZERO);
        let idp = Counting::new(&["k1"]);
        cache.key(URL, named("k1"), || idp.fetch()).await.unwrap();
        *idp.keys.lock().unwrap() = vec!["k2"];

        let key = cache.key(URL, named("k2"), || idp.fetch()).await;

        assert_eq!(key, Ok(Some("k2")));
        assert_eq!(idp.fetches(), 2);
        assert_eq!(cache.last(URL).as_deref(), Some(&vec!["k2"]));
    }

    #[tokio::test]
    async fn a_stale_set_is_fetched_again_even_for_a_known_key() {
        let cache = JwksCache::new(Some(Duration::ZERO), Duration::ZERO);
        let idp = Counting::new(&["k1"]);

        cache.key(URL, named("k1"), || idp.fetch()).await.unwrap();
        cache.key(URL, named("k1"), || idp.fetch()).await.unwrap();

        assert_eq!(idp.fetches(), 2);
    }

    #[tokio::test]
    async fn a_failed_fetch_is_the_callers_error_and_caches_nothing() {
        let cache: JwksCache<Vec<&str>> = JwksCache::new(None, Duration::from_secs(60));

        let key = cache
            .key(URL, |_| Some(()), || async { Err::<Vec<&str>, _>("down") })
            .await;

        assert_eq!(key, Err("down"));
        assert!(cache.last(URL).is_none());
    }
}
