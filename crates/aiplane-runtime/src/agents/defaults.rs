// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The pools an agent falls back to when its spec names none: the gateway's
//! admin "Default models" (`feature_defaults`), resolved against the access
//! that bounds the caller — an agent's grants, or a manager's groups. Agents
//! have no model settings of their own besides the optional Fast/Balanced/
//! Thorough choice, whose "Balanced" is this chat default when unset.

use aiplane_core::server::feature_defaults::{self, Feature, PoolDefault};
use aiplane_core::server::principal::SystemPrincipal;
use aiplane_core::server::upstreams::PoolAccess;

use super::spec::model::VoiceSpec;
use crate::rama_server::state::RamaState;

/// A direction of `publish.voice`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceDirection {
    /// The visitor speaks; the recording is transcribed.
    Input,
    /// Answers are read aloud.
    Output,
}

impl VoiceDirection {
    pub fn feature(self) -> Feature {
        match self {
            Self::Input => Feature::Transcription,
            Self::Output => Feature::Speech,
        }
    }

    fn named(self, voice: &VoiceSpec) -> Option<&str> {
        match self {
            Self::Input => voice.input_pool(),
            Self::Output => voice.output_pool(),
        }
    }

    fn on(self, voice: &VoiceSpec) -> bool {
        match self {
            Self::Input => voice.input,
            Self::Output => voice.output,
        }
    }
}

/// The chat pool a caller with `access` gets when nothing names one: the
/// admin's "Balanced" choice (`agents.pool_balanced`) when set and reachable,
/// else the pool of the gateway's default chat model.
pub async fn chat_pool(state: &RamaState, access: &PoolAccess) -> Option<PoolDefault> {
    let balanced = state.config().agents.pool_balanced.clone();
    if let Some(pool) = balanced.as_deref()
        && let Some(model) = super::profile::pool_model(state, pool, access)
    {
        return Some(PoolDefault {
            pool: pool.to_string(),
            model,
        });
    }
    feature_defaults::default_pool(&state.db, &state.upstreams, Feature::Chat, access).await
}

/// The pool and model a direction of `voice` runs on for agent `principal`,
/// or `None` when it is off or nothing it may reach serves it. The pool the
/// spec names when it names one, else the gateway's default model for that
/// direction — always among the pools granted to the agent.
pub async fn voice_pool(
    state: &RamaState,
    principal: &SystemPrincipal,
    voice: &VoiceSpec,
    direction: VoiceDirection,
) -> Option<PoolDefault> {
    if !direction.on(voice) {
        return None;
    }
    let access = match direction.named(voice) {
        Some(pool) => PoolAccess::for_system_pools(principal, [pool]),
        None => PoolAccess::for_system(principal),
    };
    feature_defaults::default_pool(&state.db, &state.upstreams, direction.feature(), &access).await
}

/// The pool the gateway's default for `feature` resolves to among the pools
/// `granted` names: what the validator checks a voice direction without a
/// pool against before publishing.
pub async fn granted_default(
    state: &RamaState,
    feature: Feature,
    granted: impl IntoIterator<Item = String>,
) -> Option<String> {
    let access = PoolAccess {
        granted_pools: Some(std::sync::Arc::new(granted.into_iter().collect())),
        ..PoolAccess::all()
    };
    feature_defaults::default_pool(&state.db, &state.upstreams, feature, &access)
        .await
        .map(|d| d.pool)
}
