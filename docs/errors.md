# Error handling

Error messages are a product surface. Write them so the reader understands the attempted operation, the cause and an actionable next step. This page separates implementation conventions from the observable HTTP envelopes.

## Implementation conventions

Use `thiserror` for typed errors crossing module or API boundaries. Use `anyhow` with `.context(...)` inside application pipelines to preserve what was being attempted. Treat a panic as a programming error, not an expected response to invalid input or an unavailable upstream.

Prefer one useful context chain over repeatedly logging the same failure at every layer. Log sensitive internal details only at an appropriate boundary and return a safe message to the caller. User-visible application strings use the shared translation catalogs where the boundary supports them.

A useful message answers:

1. What operation was being attempted?
2. What went wrong?
3. What can the reader do about it?

For example, a model-access error should name the model and suggest listing accessible models or asking an administrator about permissions. This is a writing example, not a promise of the exact text every handler returns.

## HTTP envelopes

The proxy, browser JSON API and Anthropic compatibility layer have separate protocol boundaries; there is no single `server::api::error::IntoResponse` module shared by them.

| Surface | Implementation | Envelope |
| --- | --- | --- |
| OpenAI-compatible proxy | `crates/aiplane/src/rama_server/proxy.rs` | `{"error":{"message":"…","type":"…","code":"…"}}` for gateway-generated errors |
| Browser JSON API | `crates/aiplane-api/src/pages/mod.rs`, `json_error` and related helpers | `{"error":{"message":"…","type":"…","code":"…"}}`, with optional additional fields |
| Anthropic compatibility | `crates/aiplane-core/src/server/anthropic/error.rs` and `crates/aiplane/src/rama_server/messages.rs` | Anthropic-shaped protocol errors |

In the proxy's general error helper, `type` and `code` use the supplied code. Model-not-found has the more specific OpenAI request-error shape, including `param: model`. Upstream error bodies can be relayed rather than rewritten. See [Gateway HTTP API](gateway-api.md#errors) and [Claude Code](claude-code.md) for the client contracts.

Streaming errors must follow the stream's protocol: an HTTP status cannot be replaced after response headers have been sent. Clients should inspect terminal error events/chunks as well as the original HTTP status.

## Sensitive data

Do not include passwords, bearer credentials, client secrets or raw provider keys in returned messages or logs. Use the existing secret-handling and logging conventions of the relevant subsystem. Include personal data only when the operation requires it and the reader has permission to receive it.

## Testing errors

Exercise the actual failure through its boundary and assert the observable status, stable code and protocol envelope. Check that sensitive information is absent. For a streaming failure, assert the terminal event/chunk and closure behavior. Use state-based tests and real in-memory/database or mock upstream collaborators according to [the testing strategy](testing.md).
