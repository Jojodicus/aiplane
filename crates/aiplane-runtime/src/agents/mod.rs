// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The agent builder's runtime half (`docs/agents.md`): the spec layout and
//! its validator, typed conversation state with the generated `set_<slot>`
//! tools, the route gates, the router and sub-agent dispatch behind
//! `forward_request`, bound arguments, the `RunProfile` that drives one
//! agent's live version through the ordinary headless loop, and the seam the
//! public endpoint runs a visitor's turn through (`embed`), with its visitor
//! limits and owner budget, and the retention sweeper (`retention`). Human in
//! the loop is `approval` (per-tool `always_ask`), `human` (handoffs) and
//! `inbox` (who answers what, and the notification).

pub mod approval;
pub mod bind;
pub mod embed;
pub mod gate;
pub mod human;
pub mod inbox;
pub mod output_filter;
pub mod profile;
pub mod resume;
pub mod retention;
pub mod router;
pub mod run;
pub mod slot_tools;
pub mod spec;
pub mod state;
