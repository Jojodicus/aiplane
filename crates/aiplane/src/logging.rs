// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The process log filter.
//!
//! Whatever `RUST_LOG` says, `rmcp::service` stays at `debug`: at `trace` it
//! dumps every outgoing MCP request with its arguments, and a verifier's
//! `check_code` carries the code a visitor typed (`docs/agents.md` "What #95
//! built"). A target directive is more specific than a bare level, so this
//! caps `RUST_LOG=trace` and `RUST_LOG=rmcp=trace` alike.

use tracing_subscriber::EnvFilter;

/// The directive that keeps MCP arguments out of the journal.
const MCP_ARGUMENTS_STAY_OUT: &str = "rmcp::service=debug";

/// `base` with the gateway's own caps applied.
pub fn filter(base: EnvFilter) -> EnvFilter {
    base.add_directive(
        MCP_ARGUMENTS_STAY_OUT
            .parse()
            .expect("a static, valid directive"),
    )
}

/// The filter the binary runs with: `RUST_LOG`, else `info`, capped.
pub fn from_env() -> EnvFilter {
    filter(
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,aiplane=info")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_service_tracing_is_capped_whatever_the_operator_asks_for() {
        for asked in ["trace", "rmcp=trace", "info,rmcp::service=trace"] {
            let shown = filter(EnvFilter::new(asked)).to_string();
            assert!(shown.contains("rmcp::service=debug"), "{asked}: {shown}");
        }
    }
}
