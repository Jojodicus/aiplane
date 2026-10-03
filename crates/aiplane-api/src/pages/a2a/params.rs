// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The method parameters A2A callers send, checked.

use serde_json::Value;

use super::envelope::RpcError;

/// Longest message accepted, in characters — the embed endpoint's limit.
const MAX_MESSAGE_CHARS: usize = 8_000;

#[derive(Debug)]
pub(super) struct SendParams {
    pub(super) text: String,
    pub(super) context_id: Option<String>,
    pub(super) task_id: Option<String>,
    pub(super) return_immediately: bool,
    pub(super) history_length: Option<usize>,
}

fn opt_string(v: &Value, key: &str, at: &str) -> Result<Option<String>, RpcError> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s.trim().to_string())),
        Some(_) => Err(RpcError::invalid_params(format!(
            "`{at}.{key}` must be a non-empty string"
        ))),
    }
}

pub(super) fn history_length(v: Option<&Value>, at: &str) -> Result<Option<usize>, RpcError> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(n) => n.as_u64().map(|n| Some(n as usize)).ok_or_else(|| {
            RpcError::invalid_params(format!("`{at}` must be a whole number of 0 or more"))
        }),
    }
}

fn accepts_text(mode: &str) -> bool {
    matches!(mode, "text/plain" | "text/*" | "*/*")
}

pub(super) fn parse_send(params: &Value) -> Result<SendParams, RpcError> {
    let Some(message) = params.get("message").filter(|m| m.is_object()) else {
        return Err(RpcError::invalid_params(
            "`params.message` is required: the message to send",
        ));
    };
    if message.get("role").and_then(Value::as_str) != Some("ROLE_USER") {
        return Err(RpcError::invalid_params(
            "`message.role` must be `ROLE_USER`: the caller's message to the agent",
        ));
    }
    if opt_string(message, "messageId", "message")?.is_none() {
        return Err(RpcError::invalid_params("`message.messageId` is required"));
    }
    let Some(parts) = message
        .get("parts")
        .and_then(Value::as_array)
        .filter(|p| !p.is_empty())
    else {
        return Err(RpcError::invalid_params(
            "`message.parts` needs at least one text part",
        ));
    };
    let mut texts = Vec::with_capacity(parts.len());
    for (i, part) in parts.iter().enumerate() {
        if ["raw", "url", "data"]
            .iter()
            .any(|k| part.get(*k).is_some())
        {
            return Err(RpcError::content_type(format!(
                "`message.parts[{i}]` is not text — this agent accepts only text parts \
                 (`{{\"text\": \"…\"}}`, defaultInputModes `text/plain`)"
            )));
        }
        if let Some(media) = part.get("mediaType").and_then(Value::as_str)
            && !media.starts_with("text/")
        {
            return Err(RpcError::content_type(format!(
                "`message.parts[{i}].mediaType` is `{media}` — this agent accepts text/plain"
            )));
        }
        let Some(text) = part.get("text").and_then(Value::as_str) else {
            return Err(RpcError::invalid_params(format!(
                "`message.parts[{i}]` needs a `text` string"
            )));
        };
        texts.push(text);
    }
    let text = texts.join("\n").trim().to_string();
    if text.is_empty() {
        return Err(RpcError::invalid_params(
            "the message is empty — put the text in a part's `text`",
        ));
    }
    if text.chars().count() > MAX_MESSAGE_CHARS {
        return Err(RpcError::invalid_params(format!(
            "the message is longer than {MAX_MESSAGE_CHARS} characters — shorten it and send it \
             again"
        )));
    }
    let config = params.get("configuration").cloned().unwrap_or(Value::Null);
    if config
        .get("taskPushNotificationConfig")
        .is_some_and(|c| !c.is_null())
    {
        return Err(RpcError::push_not_supported(
            "this agent sends no push notifications — leave out `taskPushNotificationConfig`",
        ));
    }
    if let Some(modes) = config.get("acceptedOutputModes").and_then(Value::as_array)
        && !modes.is_empty()
        && !modes.iter().filter_map(Value::as_str).any(accepts_text)
    {
        return Err(RpcError::content_type(
            "this agent answers in text/plain only — add it to `acceptedOutputModes`",
        ));
    }
    Ok(SendParams {
        text,
        context_id: opt_string(message, "contextId", "message")?,
        task_id: opt_string(message, "taskId", "message")?,
        return_immediately: config.get("returnImmediately") == Some(&Value::Bool(true)),
        history_length: history_length(config.get("historyLength"), "configuration.historyLength")?,
    })
}

pub(super) fn task_id_param(params: &Value) -> Result<String, RpcError> {
    opt_string(params, "id", "params")?
        .ok_or_else(|| RpcError::invalid_params("`params.id` is required: the task id"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn send(message: Value) -> Result<SendParams, RpcError> {
        parse_send(&json!({ "message": message }))
    }

    fn msg(parts: Value) -> Value {
        json!({ "messageId": "m1", "role": "ROLE_USER", "parts": parts })
    }

    #[test]
    fn text_parts_are_joined_and_anything_else_is_a_content_type_error() {
        let p = send(msg(
            json!([{ "text": "hello" }, { "text": "world", "mediaType": "text/plain" }]),
        ))
        .unwrap();
        assert_eq!(p.text, "hello\nworld");
        assert!(!p.return_immediately);
        for part in [
            json!({ "url": "https://x/a.png" }),
            json!({ "raw": "AAAA" }),
            json!({ "data": { "a": 1 } }),
            json!({ "text": "x", "mediaType": "application/json" }),
        ] {
            assert_eq!(send(msg(json!([part]))).unwrap_err().code, -32005);
        }
    }

    #[test]
    fn a_message_needs_its_id_the_user_role_and_some_text() {
        assert_eq!(
            send(json!({ "role": "ROLE_USER", "parts": [{ "text": "x" }] }))
                .unwrap_err()
                .code,
            -32602
        );
        let mut agent = msg(json!([{ "text": "x" }]));
        agent["role"] = json!("ROLE_AGENT");
        assert_eq!(send(agent).unwrap_err().code, -32602);
        assert_eq!(send(msg(json!([]))).unwrap_err().code, -32602);
        assert_eq!(
            send(msg(json!([{ "text": "  " }]))).unwrap_err().code,
            -32602
        );
        let long = "x".repeat(MAX_MESSAGE_CHARS + 1);
        assert_eq!(
            send(msg(json!([{ "text": long }]))).unwrap_err().code,
            -32602
        );
    }

    #[test]
    fn configuration_is_honoured_or_refused() {
        let p = parse_send(&json!({
            "message": msg(json!([{ "text": "x" }])),
            "configuration": { "returnImmediately": true, "historyLength": 2,
                               "acceptedOutputModes": ["application/json", "text/plain"] }
        }))
        .unwrap();
        assert!(p.return_immediately);
        assert_eq!(p.history_length, Some(2));
        let push = parse_send(&json!({
            "message": msg(json!([{ "text": "x" }])),
            "configuration": { "taskPushNotificationConfig": { "url": "https://x" } }
        }));
        assert_eq!(push.unwrap_err().code, -32003);
        let json_only = parse_send(&json!({
            "message": msg(json!([{ "text": "x" }])),
            "configuration": { "acceptedOutputModes": ["application/json"] }
        }));
        assert_eq!(json_only.unwrap_err().code, -32005);
    }
}
