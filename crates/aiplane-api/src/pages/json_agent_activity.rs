// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! `/api/v0/agents/{id}/activity` — the agent's activity log (#111,
//! `docs/agents.md` → "What #111 built"): every event of its conversations,
//! sub-agent runs included, and of the agent itself, page by page, as a
//! JSONL export, and the hash-chain check.
//!
//! The log holds whole conversations — what visitors wrote, what the model
//! answered, every tool result — so it takes what reading the agent's
//! conversations takes: the agent-management permission and a share on the
//! agent (admins hold one on every agent). Responders, who answer handoffs
//! without a share, cannot read it.
//!
//! An exchange stored as a delta, or with blobs, is served as the whole
//! request it stood for (`agent_audit::Reconstructor`).

use std::collections::HashMap;
use std::sync::Arc;

use rama::bytes::Bytes;
use rama::futures::SinkExt as _;
use rama::http::header;
use rama::http::service::web::extract::State;
use rama::http::{Body, Request, Response, StatusCode};
use serde_json::json;

use super::json_agents::{agent_at, analytics_bound};
use super::json_principals::require_agent_manager;
use super::{bad_request, internal, json_ok};
use aiplane_agents::db::agent_audit::{self, ActivityQuery, AuditKind, Order, Reconstructor};
use aiplane_agents::db::agents::Access;
use aiplane_runtime::rama_server::state::RamaState;

macro_rules! or_return {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(resp) => return resp,
        }
    };
}

const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 500;
/// A page stops once this much event detail is on it, so a page of model
/// exchanges with long prompts stays a reasonable response.
const PAGE_BYTES: usize = 4 * 1024 * 1024;
/// What the export reads per query, and so about what it holds in memory.
const EXPORT_BATCH_BYTES: usize = 1024 * 1024;

/// The filters every activity route shares.
fn query_of(agent_id: &str, query: &HashMap<String, String>) -> Result<ActivityQuery, Response> {
    let kinds = match query.get("kind").filter(|k| !k.is_empty()) {
        None => Vec::new(),
        Some(list) => list
            .split(',')
            .map(|k| {
                AuditKind::parse(k.trim()).ok_or_else(|| {
                    let known: Vec<&str> = AuditKind::ALL.iter().map(|k| k.as_str()).collect();
                    bad_request(format!(
                        "`{k}` is not an activity kind — use one of {}",
                        known.join(", ")
                    ))
                })
            })
            .collect::<Result<_, _>>()?,
    };
    let from = query
        .get("from")
        .map(|v| analytics_bound("from", v, false))
        .transpose()?;
    let to = query
        .get("to")
        .map(|v| analytics_bound("to", v, true))
        .transpose()?;
    if let (Some(from), Some(to)) = (from, to)
        && from >= to
    {
        return Err(bad_request(
            "`from` must be before `to` — swap them or widen the range",
        ));
    }
    Ok(ActivityQuery {
        agent_id: agent_id.to_string(),
        conversation_id: query.get("conversation").filter(|c| !c.is_empty()).cloned(),
        kinds,
        from,
        to,
        ..ActivityQuery::default()
    })
}

fn params(req: &Request) -> HashMap<String, String> {
    serde_urlencoded::from_str(req.uri().query().unwrap_or("")).unwrap_or_default()
}

/// GET /api/v0/agents/{id}/activity?conversation=&kind=&from=&to=&cursor=&order=&limit=
/// — one page of events, newest first unless `order=asc`, with a
/// `next_cursor` to pass back while there are more.
pub async fn list(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let user = or_return!(require_agent_manager(&state, &req).await);
    let (agent, _) = or_return!(agent_at(&state, &req, &user, 1, Access::Read).await);
    let params = params(&req);
    let mut query = or_return!(query_of(&agent.principal.id, &params));
    query.order = match params.get("order").map(String::as_str) {
        None | Some("desc") => Order::Desc,
        Some("asc") => Order::Asc,
        Some(other) => {
            return bad_request(format!("`order` is `{other}`; use `asc` or `desc`"));
        }
    };
    query.cursor = match params.get("cursor").filter(|c| !c.is_empty()) {
        None => None,
        Some(c) => match c.parse::<i64>() {
            Ok(c) => Some(c),
            Err(_) => {
                return bad_request(format!(
                    "`cursor` is `{c}`; pass back the `next_cursor` of the previous page"
                ));
            }
        },
    };
    query.limit = match params.get("limit") {
        None => DEFAULT_LIMIT,
        Some(l) => match l.parse::<usize>() {
            Ok(n) if (1..=MAX_LIMIT).contains(&n) => n,
            _ => {
                return bad_request(format!(
                    "`limit` is `{l}`; it must be a whole number from 1 to {MAX_LIMIT}"
                ));
            }
        },
    };
    query.max_bytes = PAGE_BYTES;
    let page = match agent_audit::page(&state.db, &query).await {
        Ok(page) => page,
        Err(err) => return internal(err),
    };
    let mut reconstructor = Reconstructor::default();
    let mut events = Vec::with_capacity(page.events.len());
    for event in &page.events {
        match reconstructor.event_json(&state.db, event).await {
            Ok(json) => events.push(json),
            Err(err) => return internal(err),
        }
    }
    json_ok(
        StatusCode::OK,
        json!({
            "events": events,
            "next_cursor": page.next_cursor,
            "order": if query.order == Order::Asc { "asc" } else { "desc" },
        }),
    )
}

/// GET /api/v0/agents/{id}/activity/export?conversation=&kind=&from=&to=
/// — every matching event, oldest first, one JSON object per line. Streamed
/// a batch at a time with backpressure, so the gateway holds about one batch
/// however long the log is.
pub async fn export(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let user = or_return!(require_agent_manager(&state, &req).await);
    let (agent, _) = or_return!(agent_at(&state, &req, &user, 2, Access::Read).await);
    let mut query = or_return!(query_of(&agent.principal.id, &params(&req)));
    query.order = Order::Asc;
    query.limit = MAX_LIMIT;
    query.max_bytes = EXPORT_BATCH_BYTES;
    let (mut tx, rx) = rama::futures::channel::mpsc::channel::<Result<Bytes, std::io::Error>>(2);
    tokio::spawn(async move {
        let mut reconstructor = Reconstructor::default();
        loop {
            let page = match agent_audit::page(&state.db, &query).await {
                Ok(page) => page,
                Err(err) => {
                    tracing::error!(error = %err, agent = %query.agent_id, "exporting the activity log");
                    let _ = tx
                        .send(Err(std::io::Error::other(format!(
                            "reading the activity log failed: {err}"
                        ))))
                        .await;
                    return;
                }
            };
            let mut lines = String::new();
            for event in &page.events {
                let json = match reconstructor.event_json(&state.db, event).await {
                    Ok(json) => json,
                    Err(err) => {
                        tracing::error!(error = %err, agent = %query.agent_id, "exporting the activity log");
                        let _ = tx
                            .send(Err(std::io::Error::other(format!(
                                "reading the activity log failed: {err}"
                            ))))
                            .await;
                        return;
                    }
                };
                lines.push_str(&json.to_string());
                lines.push('\n');
            }
            if !lines.is_empty() && tx.send(Ok(Bytes::from(lines))).await.is_err() {
                return;
            }
            match page.next_cursor {
                Some(cursor) => query.cursor = Some(cursor),
                None => return,
            }
        }
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .header(
            header::CONTENT_DISPOSITION,
            format!(
                "attachment; filename=\"agent-{}-activity.jsonl\"",
                agent.principal.name
            ),
        )
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from_stream(rx))
        .unwrap_or_else(internal)
}

/// GET /api/v0/agents/{id}/activity/verify — walk every hash chain of the
/// agent, check each conversation against its latest anchor, and report the
/// first thing that does not hold, with the agent chain's head for an
/// operator to keep outside the gateway.
pub async fn verify(State(state): State<Arc<RamaState>>, req: Request) -> Response {
    let user = or_return!(require_agent_manager(&state, &req).await);
    let (agent, _) = or_return!(agent_at(&state, &req, &user, 2, Access::Read).await);
    match agent_audit::verify(&state.db, &agent.principal.id).await {
        Ok(v) => json_ok(
            StatusCode::OK,
            json!({
                "ok": v.ok(),
                "chains": v.chains,
                "events": v.events,
                "unchained": v.unchained,
                "unanchored": v.unanchored,
                "head": v.head,
                "broken": v.broken,
            }),
        ),
        Err(err) => internal(err),
    }
}
