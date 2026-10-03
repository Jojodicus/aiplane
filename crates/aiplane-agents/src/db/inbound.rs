// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The events a public agent's visitor rates count, across every inbound
//! channel at once (`docs/agents.md` §5, "What #92 built"). A channel is a
//! table with one row per conversation opened on it — the embed widget's
//! `visitor_sessions`, A2A's `a2a_contexts` — naming the agent, the client IP
//! and the chat session the conversation runs in. The counted events are
//! the rows a request leaves behind: a conversation opened, a message sent in
//! one. A refused request writes nothing, so it never counts.
//!
//! One shape for every channel, so a new one is a row in [`CHANNELS`], not a
//! branch in the rate gate.

use jiff::Timestamp;

use super::{DbError, Pool, window_key};

/// One conversation on an inbound channel, by the key that channel names it
/// with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inbound<'a> {
    /// An embed visitor session (`visitor_sessions.id`).
    Visitor(&'a str),
    /// An A2A context (`a2a_contexts.session_id`, its conversation's id).
    A2a(&'a str),
}

/// Where one channel keeps its conversations.
struct Channel {
    table: &'static str,
    /// The column naming the agent served.
    agent: &'static str,
    /// The column [`Inbound`] names a conversation by.
    key: &'static str,
}

const EMBED: Channel = Channel {
    table: "visitor_sessions",
    agent: "principal_id",
    key: "id",
};

const A2A: Channel = Channel {
    table: "a2a_contexts",
    agent: "agent_id",
    key: "session_id",
};

const CHANNELS: [Channel; 2] = [EMBED, A2A];

impl<'a> Inbound<'a> {
    fn channel(self) -> (Channel, &'a str) {
        match self {
            Inbound::Visitor(id) => (EMBED, id),
            Inbound::A2a(id) => (A2A, id),
        }
    }
}

fn parse_times(rows: Vec<String>) -> Vec<Timestamp> {
    rows.iter().filter_map(|t| t.parse().ok()).collect()
}

/// The newest `limit` messages sent in `conversation` at or after `since`:
/// all a per-conversation rate of at most `limit` events needs.
pub async fn message_times(
    pool: &Pool,
    conversation: Inbound<'_>,
    since: Timestamp,
    limit: u32,
) -> Result<Vec<Timestamp>, DbError> {
    let (Channel { table, key, .. }, id) = conversation.channel();
    let rows: Vec<String> = sqlx::query_scalar(&format!(
        "SELECT t.created_at FROM chat_turns t
         JOIN {table} c ON c.session_id = t.session_id
         WHERE c.{key} = ? AND t.role = 'user' AND rtrim(t.created_at, 'Z') >= ?
         ORDER BY rtrim(t.created_at, 'Z') DESC LIMIT ?"
    ))
    .bind(id)
    .bind(window_key(since))
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(parse_times(rows))
}

/// The newest `limit` of the conversations `ip` opened with `agent_id` on
/// any channel, and of the messages sent in one, at or after `since`: the
/// per-IP rate window. Opening a conversation counts too, or a client could
/// open a fresh one for each message and never meet the per-conversation
/// limit.
pub async fn ip_event_times(
    pool: &Pool,
    agent_id: &str,
    ip: &str,
    since: Timestamp,
    limit: u32,
) -> Result<Vec<Timestamp>, DbError> {
    let events = CHANNELS
        .iter()
        .map(|Channel { table, agent, .. }| {
            format!(
                "SELECT created_at FROM {table}
                 WHERE {agent} = ? AND client_ip = ? AND rtrim(created_at, 'Z') >= ?
                 UNION ALL
                 SELECT t.created_at FROM chat_turns t
                 JOIN {table} c ON c.session_id = t.session_id
                 WHERE c.{agent} = ? AND c.client_ip = ? AND t.role = 'user'
                   AND rtrim(t.created_at, 'Z') >= ?"
            )
        })
        .collect::<Vec<_>>()
        .join(" UNION ALL ");
    let sql =
        format!("SELECT created_at FROM ({events}) ORDER BY rtrim(created_at, 'Z') DESC LIMIT ?");
    let from = window_key(since);
    let mut query = sqlx::query_scalar(&sql);
    for _ in 0..CHANNELS.len() * 2 {
        query = query.bind(agent_id).bind(ip).bind(&from);
    }
    let rows: Vec<String> = query.bind(limit).fetch_all(pool).await?;
    Ok(parse_times(rows))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use jiff::SignedDuration;
    use session_core::db as chat;
    use uuid::Uuid;

    use super::*;
    use crate::db::a2a_contexts::{self, NewContext};
    use crate::db::visitor_sessions::{self, NewVisitorSession};
    use crate::db::{agents, embed_keys, system_principals as sp};

    const MIN: SignedDuration = SignedDuration::from_secs(60);

    fn at(minutes: i32) -> Timestamp {
        let t0: Timestamp = "2026-10-01T12:00:00Z".parse().unwrap();
        t0.checked_add(MIN * minutes).unwrap()
    }

    async fn message_at(pool: &Pool, session_id: &str, when: Timestamp) {
        let id = Uuid::new_v4().to_string();
        chat::create_user_turn(pool, session_id, &id, "hi")
            .await
            .unwrap();
        sqlx::query("UPDATE chat_turns SET created_at = ? WHERE id = ?")
            .bind(when.to_string())
            .bind(&id)
            .execute(pool)
            .await
            .unwrap();
    }

    /// The per-IP window is one count across channels: a client that splits
    /// its traffic between the widget and A2A meets the same limit.
    #[tokio::test]
    async fn an_ips_events_on_every_channel_are_one_window() {
        let pool = aiplane_core::server::db::open(Path::new(":memory:"))
            .await
            .unwrap();
        let agent = agents::create(
            &pool,
            &sp::NewPrincipal {
                name: "support",
                display: "Support",
                description: "",
            },
            "{}",
            "alice",
        )
        .await
        .unwrap()
        .unwrap()
        .principal
        .id;
        let caller = sp::create(
            &pool,
            &sp::NewPrincipal {
                name: "partner-bot",
                display: "Partner bot",
                description: "",
            },
            "alice",
        )
        .await
        .unwrap()
        .unwrap()
        .id;
        let key = embed_keys::create(
            &pool,
            &embed_keys::NewEmbedKey {
                principal_id: &agent,
                name: "site",
                origins: &["https://a.example".to_string()],
                key_hash: "kh",
            },
            "alice",
        )
        .await
        .unwrap();
        let visitor = visitor_sessions::start(
            &pool,
            &NewVisitorSession {
                principal_id: &agent,
                embed_key_id: &key.id,
                agent_version: 1,
                token_hash: "th",
                client_ip: Some("192.0.2.1"),
                idle_ttl: 30 * MIN,
                max_age: 60 * MIN,
                now: at(0),
            },
        )
        .await
        .unwrap();
        let context = a2a_contexts::open(
            &pool,
            &NewContext {
                agent_id: &agent,
                agent_version: 1,
                caller_id: &caller,
                caller_name: "partner-bot",
                token_id: "tok-1",
                client_ip: Some("192.0.2.1"),
                now: at(1),
            },
        )
        .await
        .unwrap();
        message_at(&pool, &visitor.session_id, at(2)).await;
        message_at(&pool, &context.session_id, at(3)).await;

        assert_eq!(
            ip_event_times(&pool, &agent, "192.0.2.1", at(0), 10)
                .await
                .unwrap(),
            [at(3), at(2), at(1), at(0)]
        );
        assert_eq!(
            ip_event_times(&pool, &agent, "192.0.2.1", at(0), 3)
                .await
                .unwrap(),
            [at(3), at(2), at(1)],
            "only the newest are read, whichever channel they came on"
        );
        assert_eq!(
            message_times(&pool, Inbound::Visitor(&visitor.id), at(0), 10)
                .await
                .unwrap(),
            [at(2)]
        );
        assert_eq!(
            message_times(&pool, Inbound::A2a(&context.session_id), at(0), 10)
                .await
                .unwrap(),
            [at(3)],
            "a conversation counts only its own messages"
        );
    }
}
