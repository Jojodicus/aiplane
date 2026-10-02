// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Detector for a model that calls the same gateway tool with the same
//! arguments over and over within one turn.
//!
//! [`LoopGuard`](crate::loop_guard::LoopGuard) only sees repeated *text*. A
//! model stuck on a tool call writes no repeated text: it emits a fresh,
//! well-formed `tool_calls` entry every round, burns the whole round budget,
//! and ends with nothing to show. [`RepeatedCallGuard`] counts calls by
//! `(tool, canonical arguments)` so both tool loops (the `/v1` runner and the
//! chat driver) share one definition of "the same call".

use aiplane_core::server::tool_args::tool_arguments_object;
use serde_json::Value;

/// Identical calls that run normally. Two identical calls are routine (a
/// retry after a transient failure, a re-read after a write); a third is
/// already suspicious; a fourth means the model is not using the result it
/// has.
pub const MAX_IDENTICAL_CALLS: u32 = 3;

/// Identical calls past [`MAX_IDENTICAL_CALLS`] answered with a refusal before
/// the turn is stopped. The refusal is the model's one chance to change course
/// with a message that tells it so; two chances show it cannot.
pub const MAX_REFUSED_CALLS: u32 = 2;

/// What to do with one call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallVerdict {
    Run,
    Refuse,
    Stop,
}

/// Returned in place of a tool result when a call is refused.
pub const REFUSAL_MESSAGE: &str = "You already made this exact call several times and have its \
     result in the conversation above. Do not call it again: use that result, or try a different \
     call, or answer with what you have.";

/// The reason a turn was stopped, for the user, the log and the `/v1` signal.
///
/// English-only, like `loop_guard::LOOP_MESSAGE`: it is written into the turn
/// row at generation time, where the reader's locale is not known.
pub fn stop_message(tool: &str) -> String {
    format!(
        "The response was stopped because the model kept calling `{tool}` with identical \
         arguments (repeated tool call detected)."
    )
}

/// Per-turn state; create one at the start of a turn and drop it at the end.
#[derive(Default)]
pub struct RepeatedCallGuard {
    last: Option<((String, String), u32)>,
}

impl RepeatedCallGuard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn observe(&mut self, tool: &str, arguments_raw: &str) -> CallVerdict {
        let key = (
            tool.to_string(),
            canonical_json(&tool_arguments_object(arguments_raw)),
        );
        // Consecutive only: edit -> read -> edit -> read repeats a call
        // legitimately because the result changes in between.
        let seen = match &mut self.last {
            Some((last, n)) if *last == key => {
                *n += 1;
                *n
            }
            _ => {
                self.last = Some((key, 1));
                1
            }
        };
        if seen <= MAX_IDENTICAL_CALLS {
            CallVerdict::Run
        } else if seen <= MAX_IDENTICAL_CALLS + MAX_REFUSED_CALLS {
            CallVerdict::Refuse
        } else {
            CallVerdict::Stop
        }
    }
}

/// Compact JSON with object keys sorted at every depth, so key order and
/// whitespace in the model's argument string never make two calls differ.
fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let fields: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", Value::String(k.clone()), canonical_json(&map[k])))
                .collect();
            format!("{{{}}}", fields.join(","))
        }
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", items.join(","))
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARGS: &str = r#"{"q":"rust"}"#;

    #[test]
    fn calls_up_to_the_threshold_run() {
        let mut g = RepeatedCallGuard::new();
        for _ in 0..MAX_IDENTICAL_CALLS {
            assert_eq!(g.observe("search", ARGS), CallVerdict::Run);
        }
    }

    #[test]
    fn calls_past_the_threshold_are_refused_then_the_turn_stops() {
        let mut g = RepeatedCallGuard::new();
        for _ in 0..MAX_IDENTICAL_CALLS {
            g.observe("search", ARGS);
        }
        for _ in 0..MAX_REFUSED_CALLS {
            assert_eq!(g.observe("search", ARGS), CallVerdict::Refuse);
        }
        assert_eq!(g.observe("search", ARGS), CallVerdict::Stop);
    }

    #[test]
    fn key_order_and_whitespace_do_not_make_calls_different() {
        let mut g = RepeatedCallGuard::new();
        let spellings = [
            r#"{"a":1,"b":{"x":[1,2],"y":null}}"#,
            r#"{ "b": {"y": null, "x": [1, 2]}, "a": 1 }"#,
            "{\n  \"a\": 1,\n  \"b\": {\"x\": [1,2], \"y\": null}\n}",
        ];
        for i in 0..MAX_IDENTICAL_CALLS as usize {
            assert_eq!(g.observe("t", spellings[i % 3]), CallVerdict::Run);
        }
        assert_eq!(g.observe("t", spellings[0]), CallVerdict::Refuse);
    }

    #[test]
    fn empty_and_garbage_arguments_count_as_the_same_empty_call() {
        let mut g = RepeatedCallGuard::new();
        let spellings = ["", "{}", "not json"];
        for i in 0..MAX_IDENTICAL_CALLS as usize {
            g.observe("t", spellings[i % 3]);
        }
        assert_eq!(g.observe("t", "{}"), CallVerdict::Refuse);
    }

    #[test]
    fn different_arguments_never_trip() {
        let mut g = RepeatedCallGuard::new();
        for i in 0..50 {
            assert_eq!(
                g.observe("search", &format!(r#"{{"q":{i}}}"#)),
                CallVerdict::Run
            );
        }
    }

    #[test]
    fn different_tools_with_the_same_arguments_are_counted_apart() {
        let mut g = RepeatedCallGuard::new();
        for _ in 0..MAX_IDENTICAL_CALLS {
            g.observe("a", ARGS);
        }
        assert_eq!(g.observe("b", ARGS), CallVerdict::Run);
    }

    #[test]
    fn array_order_is_significant() {
        let mut g = RepeatedCallGuard::new();
        for _ in 0..MAX_IDENTICAL_CALLS {
            g.observe("t", r#"{"v":[1,2]}"#);
        }
        assert_eq!(g.observe("t", r#"{"v":[2,1]}"#), CallVerdict::Run);
    }

    #[test]
    fn alternating_edit_and_read_never_trips() {
        let mut g = RepeatedCallGuard::new();
        for _ in 0..10 {
            assert_eq!(
                g.observe("read_document", r#"{"id":"d1"}"#),
                CallVerdict::Run
            );
            assert_eq!(
                g.observe("edit_document", r#"{"id":"d1"}"#),
                CallVerdict::Run
            );
        }
    }

    #[test]
    fn an_interleaved_different_call_resets_the_count() {
        let mut g = RepeatedCallGuard::new();
        for _ in 0..MAX_IDENTICAL_CALLS {
            g.observe("t", ARGS);
        }
        g.observe("other", ARGS);
        for _ in 0..MAX_IDENTICAL_CALLS {
            assert_eq!(g.observe("t", ARGS), CallVerdict::Run);
        }
        assert_eq!(g.observe("t", ARGS), CallVerdict::Refuse);
    }
}
