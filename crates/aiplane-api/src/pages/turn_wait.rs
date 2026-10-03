// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The pacing of a stream that waits for one agent turn to end — the embed
//! widget's event stream and A2A's task stream. The answer is delivered
//! whole, so the stream has nothing to say until the turn's worker leaves the
//! session worker registry ([`released`]); it reads the turn then, and
//! otherwise only keeps the connection alive.

use std::time::Duration;

use aiplane_runtime::agents::embed::released;
use session_core::SessionWorkers;
use session_core::chat_json::SseTx;
use tokio::time::Instant;

/// A comment at this interval keeps proxies and clients from treating a
/// long-running turn as a dead connection.
const KEEPALIVE: Duration = Duration::from_secs(15);

/// A stream ends after this long even if the turn still runs; the client
/// attaches again and gets a fresh snapshot.
const STREAM_LIMIT: Duration = Duration::from_secs(600);

pub(crate) enum Waited {
    /// No worker holds the turn any more: read it.
    Released,
    /// The stream ran for its whole time; end it.
    Expired,
    /// The client went away.
    Gone,
}

/// Which turn a stream waits for: turn `turn_id` of `principal_id`'s
/// conversation `session_id`.
pub(crate) struct TurnWait<'a> {
    pub(crate) workers: &'a SessionWorkers,
    pub(crate) principal_id: &'a str,
    pub(crate) session_id: &'a str,
    pub(crate) turn_id: &'a str,
}

impl TurnWait<'_> {
    /// Whether a worker holds the turn right now.
    pub(crate) fn held(&self) -> bool {
        self.workers
            .holds(self.principal_id, self.session_id, self.turn_id)
    }

    pub(crate) async fn next(&self, tx: &SseTx, started: Instant) -> Waited {
        let done = released(
            self.workers,
            self.principal_id,
            self.session_id,
            self.turn_id,
        );
        tokio::pin!(done);
        loop {
            let left = STREAM_LIMIT.saturating_sub(started.elapsed());
            if left.is_zero() {
                return Waited::Expired;
            }
            let woke = tokio::time::timeout(KEEPALIVE.min(left), &mut done).await;
            if tx.is_closed() {
                return Waited::Gone;
            }
            if woke.is_ok() {
                return Waited::Released;
            }
            let _ = tx.unbounded_send(Ok(rama::bytes::Bytes::from_static(b": working\n\n")));
        }
    }
}
