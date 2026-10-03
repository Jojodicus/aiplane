// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Operator-configurable per-feature default models.
//!
//! Each feature (chat, voice/transcription, speech output, image generation)
//! pre-selects one model. Historically that was just the alphabetically-first model the
//! pool advertised; this module lets an operator override it from
//! `/admin/models`, persisting the choice in the [`app_settings`] KV table.
//!
//! The chosen id is always *resolved against the live advertised set* before
//! use: a setting that is absent, empty, or names a model no longer being
//! served falls back to the first advertised model — exactly the pre-existing
//! behaviour — so defaults degrade gracefully across redeploys and backend
//! changes. This mirrors `pages::feedback::resolve_model`.
//!
//! [`app_settings`]: crate::server::db::app_settings

use crate::server::db::{Pool, app_settings};
use crate::server::upstreams::{PoolAccess, PoolKind, UpstreamRegistry};

/// The features that carry a configurable default model. The wire name (used
/// in the admin form and the `app_settings` key) is stable; adding a variant
/// is the only change needed to expose a new feature's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    Chat,
    Transcription,
    /// Text-to-speech: what an agent's spoken answers run on when its spec
    /// names no speech pool, and the gateway's own read-aloud.
    Speech,
    Image,
    /// The embedding model pre-selected in the RAG collection form. Unlike the
    /// other features this is *only* a UI pre-fill: the model is committed per
    /// collection and never used as an implicit fallback, so it can't silently
    /// mix incompatible vectors into an existing index.
    Embedding,
}

impl Feature {
    /// Parse the wire name posted by the admin form. Unknown names return
    /// `None` so the handler can reject them.
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "chat" => Some(Self::Chat),
            "transcription" => Some(Self::Transcription),
            "speech" => Some(Self::Speech),
            "image" => Some(Self::Image),
            "embedding" => Some(Self::Embedding),
            _ => None,
        }
    }

    /// Stable wire name (admin form field + persisted key suffix).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Transcription => "transcription",
            Self::Speech => "speech",
            Self::Image => "image",
            Self::Embedding => "embedding",
        }
    }

    /// The pool whose models this feature picks from.
    pub fn pool_kind(self) -> PoolKind {
        match self {
            Self::Chat => PoolKind::Chat,
            Self::Transcription => PoolKind::Transcription,
            Self::Speech => PoolKind::Speech,
            Self::Image => PoolKind::Image,
            Self::Embedding => PoolKind::Embedding,
        }
    }

    /// Every feature, in the order the admin page lists them.
    pub const ALL: [Self; 5] = [
        Self::Chat,
        Self::Transcription,
        Self::Speech,
        Self::Image,
        Self::Embedding,
    ];

    /// The `app_settings` key the default is stored under.
    fn key(self) -> &'static str {
        match self {
            Self::Chat => "default_model.chat",
            Self::Transcription => "default_model.transcription",
            Self::Speech => "default_model.speech",
            Self::Image => "default_model.image",
            Self::Embedding => "default_model.embedding",
        }
    }
}

/// The raw configured default for a feature, or `None` if unset. Empty stored
/// values are treated as unset. Does *not* check the id is still served — use
/// [`resolve`] or [`promote`] for that.
pub async fn get(pool: &Pool, feature: Feature) -> Option<String> {
    match app_settings::get(pool, feature.key()).await {
        Ok(Some(v)) if !v.trim().is_empty() => Some(v),
        Ok(_) => None,
        Err(err) => {
            tracing::warn!(error = %err, feature = feature.as_str(), "feature_defaults: get failed");
            None
        }
    }
}

/// Persist (`Some`) or clear (`None`) a feature's default model.
pub async fn set(
    pool: &Pool,
    feature: Feature,
    model: Option<&str>,
) -> Result<(), crate::server::db::DbError> {
    match model {
        Some(m) if !m.trim().is_empty() => app_settings::set(pool, feature.key(), m.trim()).await,
        _ => app_settings::delete(pool, feature.key()).await,
    }
}

/// Resolve a configured id against the live advertised set: honour it if it's
/// actually being served, else fall back to the first advertised model (or
/// `None` when the pool is empty). The single source of truth for "which model
/// is the default for this feature right now".
pub fn resolve(configured: Option<&str>, available: &[String]) -> Option<String> {
    configured
        .filter(|m| !m.is_empty() && available.iter().any(|a| a == m))
        .map(str::to_string)
        .or_else(|| available.first().cloned())
}

/// Move the resolved default to the front of `items` in place, so callers that
/// treat "first entry" as "the default" (the chat/voice pickers, which have no
/// separate `selected` state) pre-select the operator's choice. `id_of` reads
/// the model id from each item, so this works for both bare id lists and richer
/// option structs. A configured-but-unavailable id is a no-op (the list keeps
/// its existing first entry).
pub fn promote<T>(configured: Option<&str>, items: &mut [T], id_of: impl Fn(&T) -> &str) {
    let Some(id) = configured.filter(|m| !m.is_empty()) else {
        return;
    };
    if let Some(pos) = items.iter().position(|it| id_of(it) == id) {
        items[..=pos].rotate_right(1);
    }
}

/// A pool, and the model of it a feature's default resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolDefault {
    pub pool: String,
    pub model: String,
}

/// The pool `feature`'s default model runs on for a caller with `access`:
/// [`resolve`] over the models of the pools of the feature's kind that
/// `access` reaches, then the first of those pools (by name) serving the
/// model. `None` when `access` reaches no such pool serving anything.
///
/// The one answer to "which pool, when nothing names one": an agent's voice
/// directions, a new agent's chat pool and the prompt assistant all ask it,
/// each with the access that bounds them (an agent's grants, a manager's
/// groups).
pub async fn default_pool(
    db: &Pool,
    upstreams: &UpstreamRegistry,
    feature: Feature,
    access: &PoolAccess,
) -> Option<PoolDefault> {
    let configured = get(db, feature).await;
    let kind = feature.pool_kind();
    let mut pools: Vec<(String, Vec<String>)> = upstreams
        .pools()
        .into_iter()
        .filter(|p| p.kind == kind && access.allows(p))
        .map(|p| (p.name.clone(), pool_models(&p)))
        .collect();
    pools.sort();
    pick_pool(configured.as_deref(), &pools)
}

/// The models a pool offers, the operator's declared ones first (a cloud
/// provider lists its whole catalogue on `/models`), then what its healthy
/// backends report, sorted.
pub fn pool_models(pool: &crate::server::upstreams::Pool) -> Vec<String> {
    let mut live: Vec<String> = pool
        .backends
        .iter()
        .filter(|b| b.is_available())
        .flat_map(|b| b.models_snapshot())
        .collect();
    live.sort();
    let mut models = pool.configured_models.clone();
    for model in live {
        if !models.contains(&model) {
            models.push(model);
        }
    }
    models
}

/// [`default_pool`] over `(pool, its models)`, in the order given.
pub fn pick_pool(configured: Option<&str>, pools: &[(String, Vec<String>)]) -> Option<PoolDefault> {
    let available: Vec<String> = pools.iter().flat_map(|(_, m)| m.iter().cloned()).collect();
    let model = resolve(configured, &available)?;
    let (pool, _) = pools.iter().find(|(_, m)| m.contains(&model))?;
    Some(PoolDefault {
        pool: pool.clone(),
        model,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn feature_wire_names_round_trip() {
        for f in Feature::ALL {
            assert_eq!(Feature::from_wire(f.as_str()), Some(f));
        }
        assert_eq!(Feature::from_wire("bogus"), None);
    }

    #[test]
    fn resolve_honours_available_configured() {
        let avail = v(&["a", "b", "c"]);
        assert_eq!(resolve(Some("b"), &avail).as_deref(), Some("b"));
    }

    #[test]
    fn resolve_falls_back_when_configured_absent() {
        let avail = v(&["a", "b"]);
        assert_eq!(resolve(Some("gone"), &avail).as_deref(), Some("a"));
        assert_eq!(resolve(Some(""), &avail).as_deref(), Some("a"));
        assert_eq!(resolve(None, &avail).as_deref(), Some("a"));
    }

    #[test]
    fn resolve_none_on_empty_pool() {
        assert_eq!(resolve(Some("x"), &[]), None);
        assert_eq!(resolve(None, &[]), None);
    }

    fn pools(xs: &[(&str, &[&str])]) -> Vec<(String, Vec<String>)> {
        xs.iter().map(|(p, m)| (p.to_string(), v(m))).collect()
    }

    #[test]
    fn pick_pool_finds_the_pool_serving_the_configured_default() {
        let reachable = pools(&[("cloud", &["tts-1"]), ("local", &["kokoro", "piper"])]);
        assert_eq!(
            pick_pool(Some("piper"), &reachable),
            Some(PoolDefault {
                pool: "local".into(),
                model: "piper".into()
            })
        );
    }

    #[test]
    fn pick_pool_falls_back_to_the_first_reachable_model_like_resolve() {
        let reachable = pools(&[("cloud", &["tts-1"]), ("local", &["kokoro"])]);
        assert_eq!(
            pick_pool(Some("not-reachable"), &reachable).map(|d| d.pool),
            Some("cloud".into())
        );
        assert_eq!(
            pick_pool(None, &reachable).map(|d| d.model),
            Some("tts-1".into())
        );
    }

    #[test]
    fn pick_pool_is_none_without_a_reachable_model() {
        assert_eq!(pick_pool(Some("x"), &[]), None);
        assert_eq!(pick_pool(None, &pools(&[("empty", &[])])), None);
    }

    #[test]
    fn promote_moves_configured_to_front_preserving_rest_order() {
        let mut items = v(&["a", "b", "c", "d"]);
        promote(Some("c"), &mut items, |s| s.as_str());
        assert_eq!(items, v(&["c", "a", "b", "d"]));
    }

    #[test]
    fn promote_is_noop_when_absent_or_unset() {
        let mut items = v(&["a", "b"]);
        promote(Some("gone"), &mut items, |s| s.as_str());
        assert_eq!(items, v(&["a", "b"]));
        promote(None, &mut items, |s| s.as_str());
        assert_eq!(items, v(&["a", "b"]));
    }
}
