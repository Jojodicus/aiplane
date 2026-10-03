// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Test fixture: a tool that asks the one chatting for a code through a
//! `secure_input` suspension and says whether it matched, the way a verifier
//! (`docs/agents.md` "What #95 built") will. It never repeats the code, so the tests can show the code
//! reaching the tool and nothing else.

use std::time::Duration;

use serde_json::{Value, json};
use session_core::db::Decision;
use shared::api::ToolDef;

use super::{Tool, ToolContext, ToolError, ToolFuture};
use crate::suspend::{Suspend, SuspendRequest, tool_suspend};

pub const CHECK_CODE: &str = "check_code";

/// Asks for a code; matches it against `expected`.
pub struct CheckCode {
    pub expected: String,
    pub timeout: Duration,
}

impl CheckCode {
    pub fn new(expected: impl Into<String>, timeout: Duration) -> Self {
        Self {
            expected: expected.into(),
            timeout,
        }
    }
}

impl Tool for CheckCode {
    fn id(&self) -> &str {
        CHECK_CODE
    }

    fn schema(&self) -> ToolDef {
        ToolDef::function(
            CHECK_CODE,
            "Ask the visitor for the code they were sent, in a field only they see, and say \
             whether it matched. Takes no arguments; you never see the code.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        )
    }

    fn run<'a>(&'a self, ctx: ToolContext, _args: Value) -> ToolFuture<'a> {
        Box::pin(async move {
            match &ctx.suspend {
                Suspend::Available => Ok(tool_suspend(SuspendRequest::secure_input(
                    "Enter the code we sent you.",
                    self.timeout,
                ))),
                Suspend::Decided(_, Decision::Value { value }) => {
                    Ok(json!({ "verified": value.as_str() == Some(self.expected.as_str()) }))
                }
                Suspend::Decided(..) => Err(ToolError::Failed(
                    "no code was entered, so nothing was checked".into(),
                )),
                Suspend::Unavailable => Err(ToolError::Failed(
                    "this run cannot ask for a code. Do not retry it here.".into(),
                )),
            }
        })
    }
}
