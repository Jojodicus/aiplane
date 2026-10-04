// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Slack and Discord incoming webhooks: the outbound half of an agent's
//! notification channels (`docs/agent-hil.md`).
//!
//! A notice is a headline, one line about what waits, an optional detail and
//! a link into the inbox. Everything a visitor or the model wrote is optional
//! detail, sent only on a channel configured for it; the rest is app text
//! from the Fluent catalogs. Slack gets its `mrkdwn` control characters
//! escaped and Discord gets every mention suppressed, so a question cannot
//! ping a channel or fake a link.

use std::time::Duration;

use crate::db::agent_channels::ChannelKind;
use aiplane_core::server::outbound_guard::{self, Policy};
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

/// Where a channel may post, which follows from who configured it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// An admin's channel: the operator's own choice, like a backend. Any
    /// http(s) URL, a private or loopback host included (an internal relay),
    /// posted through the operator's client.
    Operator,
    /// A manager's channel: the service's own webhook host, reached through
    /// `outbound_guard` like every URL someone other than the operator
    /// chooses — public https, unless the operator allows private networks.
    Guarded { allow_private: bool },
}

impl Reach {
    fn policy(allow_private: bool) -> Policy {
        Policy::agent(allow_private)
    }
}

/// Post `payload` to an incoming webhook within `reach`: through `operator`
/// (the gateway's own client) for [`Reach::Operator`], through a client
/// pinned to the checked addresses otherwise. The error never contains the
/// URL: it is a credential, and the error ends up in a log line.
pub async fn post(
    operator: &reqwest::Client,
    reach: Reach,
    url: &str,
    payload: &Value,
) -> Result<(), String> {
    let request = match reach {
        Reach::Operator => operator.post(url),
        Reach::Guarded { allow_private } => {
            let pinned = outbound_guard::pin(url, Reach::policy(allow_private), POST_TIMEOUT)
                .await
                .map_err(|why| {
                    let mut why = why.replace(url, "<webhook>");
                    if let Ok(parsed) = reqwest::Url::parse(url) {
                        why = why.replace(parsed.as_str(), "<webhook>");
                    }
                    format!("the webhook's address is not one this channel may reach ({why})")
                })?;
            pinned.client.post(pinned.url)
        }
    };
    let resp = request
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

/// Check an incoming-webhook URL for `kind` within `reach` and return its
/// host, the part shown back. An admin may name any http(s) URL; anyone
/// else the service's own `https` webhook host, checked as
/// `outbound_guard` will check it on every post.
pub fn validate_webhook_url(kind: ChannelKind, raw: &str, reach: Reach) -> Result<String, String> {
    let url = reqwest::Url::parse(raw.trim())
        .map_err(|e| format!("`url` is not a URL ({e}); paste the incoming-webhook URL"))?;
    if url.username() != "" || url.password().is_some() {
        return Err("`url` must not carry a user name or password".into());
    }
    let host = url
        .host_str()
        .ok_or("`url` has no host; paste the incoming-webhook URL")?
        .to_ascii_lowercase();
    let allow_private = match reach {
        Reach::Operator => {
            return match url.scheme() {
                "https" | "http" => Ok(host),
                other => Err(format!(
                    "`url` uses `{other}`; a webhook URL is `https` (or `http` to a relay in \
                     your own network)"
                )),
            };
        }
        Reach::Guarded { allow_private } => allow_private,
    };
    outbound_guard::check_url(raw, Reach::policy(allow_private)).map_err(|why| {
        format!(
            "`url` cannot be used: {why}. Only an admin can point a channel at a host in your \
             own network"
        )
    })?;
    let expected = match kind {
        ChannelKind::Slack => host == "hooks.slack.com",
        ChannelKind::Discord => {
            matches!(host.as_str(), "discord.com" | "discordapp.com")
                && url.path().starts_with("/api/webhooks/")
        }
    };
    if !expected {
        return Err(match kind {
            ChannelKind::Slack => "a Slack incoming webhook is on `https://hooks.slack.com/…`; \
                                   create one under the Slack app's Incoming Webhooks (an admin \
                                   can point a channel at any other relay)"
                .into(),
            ChannelKind::Discord => {
                "a Discord webhook is on `https://discord.com/api/webhooks/…`; create one under \
                 the channel's Integrations (an admin can point a channel at any other relay)"
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

    const MANAGER: Reach = Reach::Guarded {
        allow_private: false,
    };

    #[test]
    fn a_managers_webhook_url_must_be_the_services_own_and_https() {
        assert_eq!(
            validate_webhook_url(
                ChannelKind::Slack,
                "https://hooks.slack.com/services/T/B/x",
                MANAGER
            ),
            Ok("hooks.slack.com".into())
        );
        for (kind, refused) in [
            (ChannelKind::Slack, "https://evil.example/hook"),
            (ChannelKind::Slack, "http://hooks.slack.com/x"),
            (ChannelKind::Discord, "https://discord.com/channels/1"),
            (ChannelKind::Discord, "http://127.0.0.1:9/api/webhooks/1/x"),
            (ChannelKind::Slack, "https://localhost/hook"),
            (ChannelKind::Slack, "not a url"),
        ] {
            assert!(
                validate_webhook_url(kind, refused, MANAGER).is_err(),
                "{refused}"
            );
        }
        assert!(
            validate_webhook_url(
                ChannelKind::Discord,
                "https://discord.com/api/webhooks/1/abc",
                MANAGER
            )
            .is_ok()
        );
        let local = validate_webhook_url(ChannelKind::Discord, "http://127.0.0.1:9/hook", MANAGER)
            .unwrap_err();
        assert!(local.contains("admin"), "the refusal says who can: {local}");
    }

    #[test]
    fn an_admins_webhook_url_may_name_any_relay() {
        assert_eq!(
            validate_webhook_url(
                ChannelKind::Discord,
                "http://127.0.0.1:9/relay",
                Reach::Operator
            ),
            Ok("127.0.0.1".into())
        );
        assert!(
            validate_webhook_url(
                ChannelKind::Slack,
                "https://relay.internal/x",
                Reach::Operator
            )
            .is_ok()
        );
        assert!(
            validate_webhook_url(
                ChannelKind::Slack,
                "ftp://relay.internal/x",
                Reach::Operator
            )
            .is_err()
        );
        assert!(
            validate_webhook_url(
                ChannelKind::Slack,
                "https://u:p@relay.internal/x",
                Reach::Operator
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn a_managers_channel_never_posts_into_the_gateways_network() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let url = format!("{}/secret-hook", server.uri());
        let err = post(
            &reqwest::Client::new(),
            MANAGER,
            &url,
            &slack_payload(&notice(None)),
        )
        .await
        .unwrap_err();
        assert!(err.contains("may reach"), "{err}");
        assert!(!err.contains("secret-hook"), "{err}");
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
        post(
            &http,
            Reach::Operator,
            &format!("{}/hook", server.uri()),
            &payload,
        )
        .await
        .unwrap();
        let err = post(
            &http,
            Reach::Operator,
            &format!("{}/gone-secret", server.uri()),
            &payload,
        )
        .await
        .unwrap_err();
        assert!(err.contains("404"), "{err}");
        assert!(!err.contains("gone-secret"), "{err}");
    }
}
