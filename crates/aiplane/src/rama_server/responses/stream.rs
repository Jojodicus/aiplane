// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Streamed chat-completion chunks → the Responses SSE event sequence.
//!
//! ```text
//! response.created, response.in_progress
//!   response.output_item.added (reasoning)  response.reasoning_text.delta …  response.reasoning_text.done  response.output_item.done
//!   response.output_item.added (message)    response.content_part.added
//!                                           response.output_text.delta …     response.output_text.done
//!                                           response.content_part.done       response.output_item.done
//!   response.output_item.added (function_call)  response.function_call_arguments.delta/.done  response.output_item.done
//! response.completed   (or response.incomplete / response.failed)
//! ```
//!
//! Every event carries a `sequence_number`, assigned by [`SequenceNumbers`] as
//! the frame leaves for the client rather than when it is built: the
//! keep-alive — a re-sent `response.in_progress`, which a client watching for
//! *events* counts where an SSE comment would be invisible to it — is produced
//! on another task, and numbering at the exit is what keeps the numbers in the
//! order the client sees.
//!
//! As on `/v1/messages`, tool calls arrive whole: the gateway's tool loop
//! withholds tool-call deltas until it knows whether it runs the call itself,
//! so a client-owned call is emitted complete, with a single arguments delta.

use std::collections::BTreeSet;

use rama::bytes::Bytes;
use serde_json::{Value, json};

use aiplane_core::server::sse::ChatDelta;

use super::output::{self, End, Shell, Usage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Reasoning,
    Message,
}

#[derive(Debug)]
struct OpenItem {
    kind: Kind,
    id: String,
    output_index: usize,
    text: String,
}

/// Encodes one response as SSE. Feed it upstream chunks; it hands back the
/// frames to write to the client, in order, and keeps the finished output
/// items for the closing event and for storage.
#[derive(Debug)]
pub struct StreamEncoder {
    shell: Shell,
    custom_tools: BTreeSet<String>,
    started: bool,
    finished: bool,
    open: Option<OpenItem>,
    output: Vec<Value>,
    usage: Usage,
    finish_reason: Option<String>,
    /// The final response object, once [`Self::complete`] built it — the one
    /// value both stored and sent with `response.completed`.
    completed: Option<Value>,
}

impl StreamEncoder {
    pub fn new(shell: Shell, custom_tools: BTreeSet<String>) -> Self {
        Self {
            shell,
            custom_tools,
            started: false,
            finished: false,
            open: None,
            output: Vec::new(),
            usage: Usage::default(),
            finish_reason: None,
            completed: None,
        }
    }

    /// `response.created` and `response.in_progress`, once. Sent before the
    /// first backend answers, so the client sees the response id at once.
    pub fn start(&mut self) -> Vec<String> {
        if self.started {
            return Vec::new();
        }
        self.started = true;
        let response = self.shell.response("in_progress", &[], &End::default());
        vec![
            frame("response.created", json!({"response": response})),
            frame("response.in_progress", json!({"response": response})),
        ]
    }

    /// The keep-alive frame: `response.in_progress` again.
    pub fn keep_alive(&self) -> String {
        let response = self.shell.response("in_progress", &[], &End::default());
        frame("response.in_progress", json!({"response": response}))
    }

    /// One upstream round's final token counts. Output sums across rounds;
    /// input is replaced, because each round resends the whole conversation.
    pub fn absorb_round_usage(&mut self, input_tokens: i64, output_tokens: i64) {
        if input_tokens > 0 {
            self.usage.input_tokens = input_tokens;
        }
        self.usage.output_tokens += output_tokens;
    }

    /// Consume one upstream `chat.completion.chunk`.
    pub fn chunk(&mut self, chunk: &Value) -> Vec<String> {
        let mut out = self.start();
        if self.finished {
            return out;
        }
        let delta = ChatDelta::new(chunk);
        if let Some(reasoning) = delta.reasoning().filter(|s| !s.is_empty()) {
            out.extend(self.delta(Kind::Reasoning, reasoning));
        }
        if let Some(text) = delta.content().filter(|s| !s.is_empty()) {
            out.extend(self.delta(Kind::Message, text));
        }
        if let Some(reason) = chunk
            .pointer("/choices/0/finish_reason")
            .and_then(Value::as_str)
        {
            // Recorded, not acted on: the loop may run another round behind
            // this same response.
            self.finish_reason = Some(reason.to_string());
        }
        out
    }

    /// A complete client-owned tool call, as one output item.
    pub fn tool_call(&mut self, call_id: &str, name: &str, arguments: &str) -> Vec<String> {
        let mut out = self.start();
        out.extend(self.close_open("completed"));
        let item = output::tool_call_item(call_id, name, arguments, &self.custom_tools);
        let output_index = self.output.len();
        let id = item["id"].as_str().unwrap_or_default().to_string();
        let mut added = item.clone();
        added["status"] = json!("in_progress");
        let (delta_event, done_event, field) = if item["type"] == "custom_tool_call" {
            (
                "response.custom_tool_call_input.delta",
                "response.custom_tool_call_input.done",
                "input",
            )
        } else {
            (
                "response.function_call_arguments.delta",
                "response.function_call_arguments.done",
                "arguments",
            )
        };
        let value = item[field].clone();
        added[field] = json!("");
        out.push(frame(
            "response.output_item.added",
            json!({"output_index": output_index, "item": added}),
        ));
        out.push(frame(
            delta_event,
            json!({"item_id": id, "output_index": output_index, "delta": value}),
        ));
        out.push(frame(
            done_event,
            json!({"item_id": id, "output_index": output_index, field: value}),
        ));
        out.push(frame(
            "response.output_item.done",
            json!({"output_index": output_index, "item": item}),
        ));
        self.output.push(item);
        out
    }

    /// Close the last open item and build the final response object, once.
    /// The frames go out with [`Self::finish`]; the object is what gets
    /// stored, before the client is told the response is complete, and what
    /// `response.completed` then carries.
    pub fn complete(&mut self) -> (Vec<String>, Value) {
        if let Some(response) = &self.completed {
            return (Vec::new(), response.clone());
        }
        let mut out = self.start();
        out.extend(self.close_open("completed"));
        let end = End {
            usage: self.usage,
            incomplete_reason: output::incomplete_reason(self.finish_reason.as_deref()),
            error: None,
        };
        let response = self.shell.response(end.status(), &self.output, &end);
        self.completed = Some(response.clone());
        (out, response)
    }

    /// The closing frames: whatever [`Self::complete`] closed, then
    /// `response.completed` (or `response.incomplete`).
    pub fn finish(&mut self) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        let (mut out, response) = self.complete();
        self.finished = true;
        let event = match response["status"].as_str() {
            Some("incomplete") => "response.incomplete",
            _ => "response.completed",
        };
        out.push(frame(event, json!({"response": response})));
        out
    }

    /// A mid-stream failure: `response.failed`, carrying what was produced so
    /// far — an item cut off by the failure as `incomplete` — and the error.
    pub fn error(&mut self, code: &str, message: &str) -> Vec<String> {
        let mut out = self.start();
        if self.finished {
            return out;
        }
        self.finished = true;
        out.extend(self.close_open("incomplete"));
        let end = End {
            usage: self.usage,
            incomplete_reason: None,
            error: Some(json!({"code": code, "message": message})),
        };
        let response = self.shell.response("failed", &self.output, &end);
        out.push(frame("response.failed", json!({"response": response})));
        out
    }

    fn delta(&mut self, kind: Kind, text: &str) -> Vec<String> {
        let mut out = Vec::new();
        if self.open.as_ref().map(|o| o.kind) != Some(kind) {
            out.extend(self.close_open("completed"));
            out.extend(self.open_item(kind));
        }
        let Some(open) = self.open.as_mut() else {
            return out;
        };
        open.text.push_str(text);
        let event = match kind {
            Kind::Reasoning => "response.reasoning_text.delta",
            Kind::Message => "response.output_text.delta",
        };
        let mut data = json!({
            "item_id": open.id,
            "output_index": open.output_index,
            "content_index": 0,
            "delta": text,
        });
        if kind == Kind::Message {
            data["logprobs"] = json!([]);
        }
        out.push(frame(event, data));
        out
    }

    fn open_item(&mut self, kind: Kind) -> Vec<String> {
        let output_index = self.output.len();
        let (id, item, part) = match kind {
            Kind::Reasoning => {
                let id = output::new_id("rs");
                let mut item = output::reasoning_item(&id, "", "in_progress");
                item["content"] = json!([]);
                (id, item, json!({"type": "reasoning_text", "text": ""}))
            }
            Kind::Message => {
                let id = output::new_id("msg");
                let mut item = output::message_item(&id, "", "in_progress");
                item["content"] = json!([]);
                (id, item, output::output_text_part(""))
            }
        };
        let frames = vec![
            frame(
                "response.output_item.added",
                json!({"output_index": output_index, "item": item}),
            ),
            frame(
                "response.content_part.added",
                json!({"item_id": id, "output_index": output_index, "content_index": 0, "part": part}),
            ),
        ];
        self.open = Some(OpenItem {
            kind,
            id,
            output_index,
            text: String::new(),
        });
        frames
    }

    /// Close the open item, if any, with `status`: `completed` when its turn
    /// went on normally, `incomplete` when a failure cut it off.
    fn close_open(&mut self, status: &str) -> Vec<String> {
        let Some(open) = self.open.take() else {
            return Vec::new();
        };
        let (done_event, item, part) = match open.kind {
            Kind::Reasoning => (
                "response.reasoning_text.done",
                output::reasoning_item(&open.id, &open.text, status),
                json!({"type": "reasoning_text", "text": open.text}),
            ),
            Kind::Message => (
                "response.output_text.done",
                output::message_item(&open.id, &open.text, status),
                output::output_text_part(&open.text),
            ),
        };
        let mut done = json!({
            "item_id": open.id,
            "output_index": open.output_index,
            "content_index": 0,
            "text": open.text,
        });
        if open.kind == Kind::Message {
            done["logprobs"] = json!([]);
        }
        let frames = vec![
            frame(done_event, done),
            frame(
                "response.content_part.done",
                json!({"item_id": open.id, "output_index": open.output_index, "content_index": 0, "part": part}),
            ),
            frame(
                "response.output_item.done",
                json!({"output_index": open.output_index, "item": item}),
            ),
        ];
        self.output.push(item);
        frames
    }
}

/// One SSE frame: the event name on its own line and as the payload's `type`.
/// The `sequence_number` is added on the way out, by [`SequenceNumbers`].
fn frame(kind: &str, mut data: Value) -> String {
    if let Some(obj) = data.as_object_mut() {
        obj.insert("type".into(), json!(kind));
    }
    format!("event: {kind}\ndata: {data}\n\n")
}

/// Numbers the frames of one stream in the order they reach the client.
#[derive(Debug, Default)]
pub struct SequenceNumbers {
    next: u64,
}

impl SequenceNumbers {
    /// `frame` with `"sequence_number": N` as its payload's last field. A
    /// frame whose payload is not a JSON object is passed through unnumbered.
    pub fn number(&mut self, frame: Bytes) -> Bytes {
        let Some(close) = frame.iter().rposition(|&b| b == b'}') else {
            return frame;
        };
        let field = format!(",\"sequence_number\":{}", self.next);
        self.next += 1;
        let mut out = Vec::with_capacity(frame.len() + field.len());
        out.extend_from_slice(&frame[..close]);
        out.extend_from_slice(field.as_bytes());
        out.extend_from_slice(&frame[close..]);
        Bytes::from(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    fn encoder() -> StreamEncoder {
        StreamEncoder::new(
            Shell::new("m", Map::new()),
            BTreeSet::from(["apply_patch".to_string()]),
        )
    }

    fn events(frames: &[String]) -> Vec<(String, Value)> {
        frames
            .iter()
            .map(|f| {
                let mut lines = f.lines();
                let name = lines
                    .next()
                    .unwrap()
                    .strip_prefix("event: ")
                    .unwrap()
                    .to_string();
                let data: Value =
                    serde_json::from_str(lines.next().unwrap().strip_prefix("data: ").unwrap())
                        .unwrap();
                assert_eq!(data["type"], name.as_str(), "the payload names its event");
                (name, data)
            })
            .collect()
    }

    fn names(frames: &[String]) -> Vec<String> {
        events(frames).into_iter().map(|(n, _)| n).collect()
    }

    fn delta(reasoning: Option<&str>, content: Option<&str>) -> Value {
        json!({"choices": [{"index": 0, "delta": {"reasoning_content": reasoning, "content": content}}]})
    }

    #[test]
    fn a_text_turn_is_framed_from_created_to_completed() {
        let mut enc = encoder();
        let mut frames = enc.start();
        frames.extend(enc.chunk(&delta(Some("hmm"), None)));
        frames.extend(enc.chunk(&delta(None, Some("Hel"))));
        frames.extend(enc.chunk(&delta(None, Some("lo"))));
        frames.extend(
            enc.chunk(&json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})),
        );
        enc.absorb_round_usage(12, 3);
        frames.extend(enc.finish());
        assert_eq!(
            names(&frames),
            [
                "response.created",
                "response.in_progress",
                "response.output_item.added",
                "response.content_part.added",
                "response.reasoning_text.delta",
                "response.reasoning_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.output_item.added",
                "response.content_part.added",
                "response.output_text.delta",
                "response.output_text.delta",
                "response.output_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.completed",
            ]
        );
        let events = events(&frames);
        let (_, completed) = events.last().unwrap();
        let response = &completed["response"];
        assert_eq!(response["status"], "completed");
        assert_eq!(response["output"][0]["content"][0]["text"], "hmm");
        assert_eq!(response["output"][1]["content"][0]["text"], "Hello");
        assert_eq!(response["usage"]["input_tokens"], 12);
        assert_eq!(response["usage"]["output_tokens"], 3);
        assert_eq!(
            events[0].1["response"]["id"], response["id"],
            "one id throughout"
        );
    }

    #[test]
    fn client_tool_calls_are_complete_items() {
        let mut enc = encoder();
        let mut frames = enc.chunk(&delta(None, Some("Patching.")));
        frames.extend(enc.tool_call("call_1", "shell", "{\"cmd\":\"ls\"}"));
        frames.extend(enc.tool_call("call_2", "apply_patch", "{\"input\":\"*** Begin Patch\"}"));
        frames.extend(enc.finish());
        let events = events(&frames);
        let added: Vec<&Value> = events
            .iter()
            .filter(|(n, _)| n == "response.output_item.added")
            .map(|(_, d)| &d["item"])
            .collect();
        assert_eq!(added[1]["type"], "function_call");
        assert_eq!(added[1]["arguments"], "");
        assert_eq!(added[1]["status"], "in_progress");
        let arguments = events
            .iter()
            .find(|(n, _)| n == "response.function_call_arguments.done")
            .unwrap();
        assert_eq!(arguments.1["arguments"], "{\"cmd\":\"ls\"}");
        let input = events
            .iter()
            .find(|(n, _)| n == "response.custom_tool_call_input.done")
            .unwrap();
        assert_eq!(input.1["input"], "*** Begin Patch");
        let output = &events.last().unwrap().1["response"]["output"];
        assert_eq!(output[0]["type"], "message");
        assert_eq!(output[1]["call_id"], "call_1");
        assert_eq!(output[2]["type"], "custom_tool_call");
    }

    #[test]
    fn a_failure_ends_with_response_failed_and_marks_the_cut_item_incomplete() {
        let mut enc = encoder();
        let mut frames = enc.chunk(&delta(None, Some("par")));
        frames.extend(enc.error("upstream_error", "backend fell over"));
        frames.extend(enc.finish());
        let events = events(&frames);
        let done = events
            .iter()
            .find(|(n, _)| n == "response.output_item.done")
            .unwrap();
        assert_eq!(done.1["item"]["status"], "incomplete");
        let (name, last) = events.last().unwrap();
        assert_eq!(name, "response.failed");
        assert_eq!(last["response"]["status"], "failed");
        assert_eq!(
            last["response"]["error"],
            json!({"code": "upstream_error", "message": "backend fell over"})
        );
        assert_eq!(last["response"]["output"][0]["content"][0]["text"], "par");
        assert_eq!(last["response"]["output"][0]["status"], "incomplete");
    }

    #[test]
    fn a_length_stop_ends_with_response_incomplete() {
        let mut enc = encoder();
        let mut frames = enc.chunk(
            &json!({"choices": [{"index": 0, "delta": {"content": "x"}, "finish_reason": "length"}]}),
        );
        frames.extend(enc.finish());
        let (name, last) = events(&frames).pop().unwrap();
        assert_eq!(name, "response.incomplete");
        assert_eq!(
            last["response"]["incomplete_details"]["reason"],
            "max_output_tokens"
        );
    }

    #[test]
    fn the_stored_response_is_the_one_announced() {
        let mut enc = encoder();
        enc.chunk(&delta(None, Some("x")));
        let (closing, stored) = enc.complete();
        assert_eq!(names(&closing).last().unwrap(), "response.output_item.done");
        let finish = enc.finish();
        assert_eq!(names(&finish), ["response.completed"]);
        assert_eq!(events(&finish)[0].1["response"], stored);
    }

    #[test]
    fn frames_are_numbered_in_the_order_they_leave() {
        let mut enc = encoder();
        let mut numbers = SequenceNumbers::default();
        let mut frames = enc.start();
        frames.push(enc.keep_alive());
        frames.extend(enc.chunk(&delta(None, Some("x"))));
        let numbered: Vec<String> = frames
            .into_iter()
            .map(|f| String::from_utf8(numbers.number(Bytes::from(f)).to_vec()).unwrap())
            .collect();
        let sequence: Vec<u64> = events(&numbered)
            .iter()
            .map(|(_, d)| d["sequence_number"].as_u64().unwrap())
            .collect();
        assert_eq!(sequence, [0, 1, 2, 3, 4, 5]);
        assert!(numbered.iter().all(|f| f.ends_with("}\n\n")));
    }
}
