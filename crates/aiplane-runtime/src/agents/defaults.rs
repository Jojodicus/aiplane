// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The models an agent runs on when its spec names none: the gateway's
//! admin "Default models" (`server::model_choices::gateway_default`).
//! Agents have no model settings of their own; an unset model key means
//! "the gateway's default", which the agent must hold a grant on like on
//! any model it names.

use aiplane_core::server::feature_defaults::Feature;

use super::spec::AgentSpec;
use super::spec::ModelDefaults;
use super::spec::model::VoiceSpec;
use crate::server::AppState;
use crate::server::model_choices::gateway_default;

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
            Self::Input => voice.input_model(),
            Self::Output => voice.output_model(),
        }
    }

    fn on(self, voice: &VoiceSpec) -> bool {
        match self {
            Self::Input => voice.input,
            Self::Output => voice.output,
        }
    }
}

/// The model the main run of `spec` uses: `main.model`, else the gateway's
/// default chat model. `None` when neither exists.
pub async fn main_model(state: &AppState, spec: &AgentSpec) -> Option<String> {
    match spec.main_model() {
        Some(model) => Some(model.to_string()),
        None => gateway_default(state, Feature::Chat).await,
    }
}

/// The model a direction of `voice` runs on, or `None` when it is off or
/// there is none: the one the spec names, else the gateway's default model
/// for that direction. Whether the agent may use it is its grants' call.
pub async fn voice_model(
    state: &AppState,
    voice: &VoiceSpec,
    direction: VoiceDirection,
) -> Option<String> {
    if !direction.on(voice) {
        return None;
    }
    match direction.named(voice) {
        Some(model) => Some(model.to_string()),
        None => gateway_default(state, direction.feature()).await,
    }
}

/// The gateway's default of each kind, which the validator checks an unset
/// model key against.
pub async fn model_defaults(state: &AppState) -> ModelDefaults {
    ModelDefaults {
        chat: gateway_default(state, Feature::Chat).await,
        transcription: gateway_default(state, Feature::Transcription).await,
        speech: gateway_default(state, Feature::Speech).await,
    }
}
