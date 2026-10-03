// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! `a2a_contexts`: one A2A context, which is one agent conversation opened by
//! a remote caller over A2A (`docs/agents.md` "What #102 built").
//!
//! The conversation is a `chat_sessions` row owned by the agent's principal,
//! exactly like a visitor's; this row says which system principal opened it,
//! with which token and from which address. Only that caller reads or
//! continues it. A context counts like a visitor session for the agent's
//! rate limits: its messages per context, and per client IP the contexts
//! opened and their messages.

use crate::db::run_sessions;
use jiff::Timestamp;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::{DbError, Pool};
use aiplane_core::server::run_chain::RemoteCaller;

/// The protocol name a context's caller is recorded under in a call chain.
pub const PROTOCOL: &str = "a2a";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct A2aContext {
    /// The conversation's `chat_sessions.id`; the A2A `contextId`.
    pub session_id: String,
    pub agent_id: String,
    pub caller_id: String,
    pub caller_name: String,
    pub token_id: String,
    pub client_ip: Option<String>,
    pub created_at: Timestamp,
}

impl A2aContext {
    /// The caller as the run's call chain records it.
    pub fn caller(&self) -> RemoteCaller {
        RemoteCaller {
            protocol: PROTOCOL.to_string(),
            principal_id: self.caller_id.clone(),
            name: self.caller_name.clone(),
            token_id: self.token_id.clone(),
        }
    }
}

pub struct NewContext<'a> {
    pub agent_id: &'a str,
    /// The live version the conversation starts on.
    pub agent_version: i64,
    pub caller_id: &'a str,
    pub caller_name: &'a str,
    pub token_id: &'a str,
    pub client_ip: Option<&'a str>,
    pub now: Timestamp,
}

const COLS: &str = "session_id, agent_id, caller_id, caller_name, token_id, client_ip, created_at";

fn map_context(row: &SqliteRow) -> Result<A2aContext, DbError> {
    Ok(A2aContext {
        session_id: row.try_get("session_id")?,
        agent_id: row.try_get("agent_id")?,
        caller_id: row.try_get("caller_id")?,
        caller_name: row.try_get("caller_name")?,
        token_id: row.try_get("token_id")?,
        client_ip: row.try_get("client_ip")?,
        created_at: super::parse_ts(row.try_get("created_at")?, "created_at")?,
    })
}

/// Open the conversation under the agent's principal and record its caller,
/// in one transaction.
pub async fn open(pool: &Pool, new: &NewContext<'_>) -> Result<A2aContext, DbError> {
    let mut tx = pool.begin().await?;
    let conversation = run_sessions::create_principal_session(
        &mut *tx,
        &run_sessions::NewRunSession {
            principal_id: new.agent_id,
            title: None,
            parent_turn_id: None,
            agent_version: Some(new.agent_version),
        },
    )
    .await?;
    sqlx::query(
        "INSERT INTO a2a_contexts (session_id, agent_id, caller_id, caller_name, token_id,
             client_ip, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&conversation.id)
    .bind(new.agent_id)
    .bind(new.caller_id)
    .bind(new.caller_name)
    .bind(new.token_id)
    .bind(new.client_ip)
    .bind(new.now.to_string())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    get(pool, &conversation.id)
        .await?
        .ok_or_else(|| DbError::Query(sqlx::Error::RowNotFound))
}

pub async fn get(pool: &Pool, session_id: &str) -> Result<Option<A2aContext>, DbError> {
    let row = sqlx::query(&format!(
        "SELECT {COLS} FROM a2a_contexts WHERE session_id = ?"
    ))
    .bind(session_id)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(map_context).transpose()
}

/// The context `session_id` of `agent_id`, when `caller_id` opened it. Any
/// other caller gets `None`, exactly as for a context that does not exist.
pub async fn get_for_caller(
    pool: &Pool,
    agent_id: &str,
    caller_id: &str,
    session_id: &str,
) -> Result<Option<A2aContext>, DbError> {
    Ok(get(pool, session_id)
        .await?
        .filter(|c| c.agent_id == agent_id && c.caller_id == caller_id))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::db::{agents, system_principals as sp};

    fn t0() -> Timestamp {
        "2026-10-01T12:00:00Z".parse().unwrap()
    }

    struct Fx {
        pool: Pool,
        agent: String,
        caller: String,
    }

    async fn fixture() -> Fx {
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
        Fx {
            pool,
            agent,
            caller,
        }
    }

    impl Fx {
        async fn open_from(&self, ip: Option<&str>, now: Timestamp) -> A2aContext {
            open(
                &self.pool,
                &NewContext {
                    agent_id: &self.agent,
                    agent_version: 2,
                    caller_id: &self.caller,
                    caller_name: "partner-bot",
                    token_id: "tok-1",
                    client_ip: ip,
                    now,
                },
            )
            .await
            .unwrap()
        }
    }

    #[tokio::test]
    async fn a_context_is_an_agent_conversation_that_remembers_its_caller() {
        let fx = fixture().await;
        let c = fx.open_from(Some("192.0.2.1"), t0()).await;
        let run = run_sessions::get_principal_session(&fx.pool, &fx.agent, &c.session_id)
            .await
            .unwrap()
            .expect("owned by the agent's principal");
        assert_eq!(run.agent_version, Some(2));
        assert_eq!(run.visitor_id, None, "an A2A caller is not a visitor");
        assert_eq!(
            c.caller(),
            RemoteCaller {
                protocol: "a2a".into(),
                principal_id: fx.caller.clone(),
                name: "partner-bot".into(),
                token_id: "tok-1".into(),
            }
        );
        assert_eq!(get(&fx.pool, &c.session_id).await.unwrap(), Some(c));
    }

    #[tokio::test]
    async fn only_the_caller_that_opened_a_context_finds_it() {
        let fx = fixture().await;
        let c = fx.open_from(None, t0()).await;
        let mine = get_for_caller(&fx.pool, &fx.agent, &fx.caller, &c.session_id)
            .await
            .unwrap();
        assert!(mine.is_some());
        for (agent, caller) in [("other-agent", fx.caller.as_str()), (&fx.agent, "other")] {
            assert_eq!(
                get_for_caller(&fx.pool, agent, caller, &c.session_id)
                    .await
                    .unwrap(),
                None
            );
        }
    }

    #[tokio::test]
    async fn the_context_goes_with_its_conversation() {
        let fx = fixture().await;
        let c = fx.open_from(None, t0()).await;
        sqlx::query("DELETE FROM chat_sessions WHERE id = ?")
            .bind(&c.session_id)
            .execute(&fx.pool)
            .await
            .unwrap();
        assert_eq!(get(&fx.pool, &c.session_id).await.unwrap(), None);
    }
}
