// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The agent builder's runtime half (`docs/agents.md`): the spec layout and
//! its validator, typed conversation state with the generated `set_<slot>`
//! tools, the route gates, the router and sub-agent dispatch behind
//! `forward_request`, bound arguments, and the `RunProfile` that drives one
//! agent's live version through the ordinary headless loop.

pub mod bind;
pub mod gate;
pub mod output_filter;
pub mod profile;
pub mod router;
pub mod run;
pub mod slot_tools;
pub mod spec;
pub mod state;
