// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Slack and Discord incoming webhooks: the outbound half of an agent's
//! notification channels (`docs/agents.md` "What #96 built").
//!
//! A notice is a headline, one line about what waits, an optional detail and
//! a link into the inbox. Everything a visitor or the model wrote is optional
//! detail, sent only on a channel configured for it; the rest is app text
//! from the Fluent catalogs. Slack gets its `mrkdwn` control characters
//! escaped and Discord gets every mention suppressed, so a question cannot
//! ping a channel or fake a link.

use std::time::Duration;

use crate::db::agent_channels::ChannelKind;
use serde_json::{Value, json};

/// How long one webhook post may take. A slow chat service must not hold
/// the notification of the next channel for long.
const POST_TIMEOUT: Duration = Duration::from_secs(10);

/// Longest detail put into a message, in characters.
const MAX_DETAIL_CHARS: usize = 300;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub headline: String,
    pub summary: String,
    /// The question or the tool name, on a channel configured with details.
    pub detail: Option<String>,
    /// Absolute URL of the inbox item.
    pub link: String,
    pub link_label: String,
}

fn detail(notice: &Notice) -> Option<String> {
    notice
        .detail
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(|d| session_core::text::truncate_chars(d, MAX_DETAIL_CHARS))
}

fn slack_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The JSON a Slack incoming webhook takes.
pub fn slack_payload(notice: &Notice) -> Value {
    let mut lines = vec![
        format!("*{}*", slack_escape(&notice.headline)),
        slack_escape(&notice.summary),
    ];
    if let Some(d) = detail(notice) {
        lines.push(format!("> {}", slack_escape(&d).replace('\n', "\n> ")));
    }
    lines.push(format!(
        "<{}|{}>",
        notice.link,
        slack_escape(&notice.link_label)
    ));
    json!({ "text": lines.join("\n") })
}

/// The JSON a Discord webhook takes. No mention in it resolves.
pub fn discord_payload(notice: &Notice) -> Value {
    let mut lines = vec![format!("**{}**", notice.headline), notice.summary.clone()];
    if let Some(d) = detail(notice) {
        lines.push(format!("> {}", d.replace('\n', "\n> ")));
    }
    lines.push(format!("{}: {}", notice.link_label, notice.link));
    json!({ "content": lines.join("\n"), "allowed_mentions": { "parse": [] } })
}

pub fn payload(kind: ChannelKind, notice: &Notice) -> Value {
    match kind {
        ChannelKind::Slack => slack_payload(notice),
        ChannelKind::Discord => discord_payload(notice),
    }
}

/// Post `payload` to an incoming webhook. The error never contains the URL:
/// it is a credential, and the error ends up in a log line.
pub async fn post(http: &reqwest::Client, url: &str, payload: &Value) -> Result<(), String> {
    let resp = http
        .post(url)
        .timeout(POST_TIMEOUT)
        .json(payload)
        .send()
        .await
        .map_err(|e| format!("the webhook could not be reached ({})", e.without_url()))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!(
            "the webhook answered {}; check that the channel's incoming webhook still exists",
            resp.status()
        ))
    }
}

/// Check an incoming-webhook URL for `kind` and return its host, the part
/// shown back. `https` only, except a loopback host for local testing.
pub fn validate_webhook_url(kind: ChannelKind, raw: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(raw.trim())
        .map_err(|e| format!("`url` is not a URL ({e}); paste the incoming-webhook URL"))?;
    let host = url
        .host_str()
        .ok_or("`url` has no host; paste the incoming-webhook URL")?
        .to_ascii_lowercase();
    let loopback = matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]");
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        other => {
            return Err(format!(
                "`url` uses `{other}`; a webhook URL must be `https`, because it carries the \
                 channel's credential"
            ));
        }
    }
    if url.username() != "" || url.password().is_some() {
        return Err("`url` must not carry a user name or password".into());
    }
    let expected = match kind {
        ChannelKind::Slack => host == "hooks.slack.com",
        ChannelKind::Discord => {
            matches!(host.as_str(), "discord.com" | "discordapp.com")
                && url.path().starts_with("/api/webhooks/")
        }
    };
    if !expected && !loopback {
        return Err(match kind {
            ChannelKind::Slack => "a Slack incoming webhook is on `https://hooks.slack.com/…`; \
                                   create one under the Slack app's Incoming Webhooks"
                .into(),
            ChannelKind::Discord => {
                "a Discord webhook is on `https://discord.com/api/webhooks/…`; \
                                     create one under the channel's Integrations"
                    .into()
            }
        });
    }
    Ok(host)
}

#[cfg(test)]
// Tests build plain clients and drain bodies to talk to their in-process
// mocks; the outbound and body rules are about production paths.
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn notice(detail: Option<&str>) -> Notice {
        Notice {
            headline: "Support is waiting".into(),
            summary: "A visitor needs an answer from a person.".into(),
            detail: detail.map(str::to_string),
            link: "https://gw.example.com/inbox?item=r1".into(),
            link_label: "Open the inbox".into(),
        }
    }

    #[test]
    fn a_slack_message_escapes_what_it_quotes_and_links_the_inbox() {
        let text = slack_payload(&notice(Some("<!channel> refund & <http://evil|click>")))["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(text.contains("&lt;!channel&gt; refund &amp; &lt;http://evil|click&gt;"));
        assert!(text.ends_with("<https://gw.example.com/inbox?item=r1|Open the inbox>"));
        let plain = slack_payload(&notice(None))["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(!plain.contains("\n> "), "no detail, no quote: {plain}");
    }

    #[test]
    fn a_discord_message_resolves_no_mention() {
        let p = discord_payload(&notice(Some("@everyone help")));
        assert_eq!(p["allowed_mentions"], json!({ "parse": [] }));
        assert!(p["content"].as_str().unwrap().contains("> @everyone help"));
    }

    #[test]
    fn a_long_detail_is_cut() {
        let long = "x".repeat(1000);
        let text = discord_payload(&notice(Some(&long)))["content"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(text.len() < 600, "{}", text.len());
    }

    #[test]
    fn a_webhook_url_must_be_the_services_own_and_https() {
        assert_eq!(
            validate_webhook_url(ChannelKind::Slack, "https://hooks.slack.com/services/T/B/x"),
            Ok("hooks.slack.com".into())
        );
        assert!(validate_webhook_url(ChannelKind::Slack, "https://evil.example/hook").is_err());
        assert!(validate_webhook_url(ChannelKind::Slack, "http://hooks.slack.com/x").is_err());
        assert!(
            validate_webhook_url(
                ChannelKind::Discord,
                "https://discord.com/api/webhooks/1/abc"
            )
            .is_ok()
        );
        assert!(
            validate_webhook_url(ChannelKind::Discord, "https://discord.com/channels/1").is_err()
        );
        assert!(validate_webhook_url(ChannelKind::Discord, "http://127.0.0.1:9/hook").is_ok());
        assert!(validate_webhook_url(ChannelKind::Slack, "not a url").is_err());
    }

    #[tokio::test]
    async fn a_post_delivers_the_payload_and_reports_a_refusal_without_the_url() {
        let server = MockServer::start().await;
        let payload = slack_payload(&notice(None));
        Mock::given(method("POST"))
            .and(path("/hook"))
            .and(body_json(payload.clone()))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        post(&http, &format!("{}/hook", server.uri()), &payload)
            .await
            .unwrap();
        let err = post(&http, &format!("{}/gone-secret", server.uri()), &payload)
            .await
            .unwrap_err();
        assert!(err.contains("404"), "{err}");
        assert!(!err.contains("gone-secret"), "{err}");
    }
}
