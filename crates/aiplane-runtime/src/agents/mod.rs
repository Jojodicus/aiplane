// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The agent builder's runtime half (`docs/agents.md`): the spec layout and
//! its validator, typed conversation state with the generated `set_<slot>`
//! tools, and (#86) the gate evaluator. Dispatch lands here with #87/#88.

pub mod slot_tools;
pub mod spec;
pub mod state;
