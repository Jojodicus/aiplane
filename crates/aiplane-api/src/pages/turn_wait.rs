// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The pacing of a stream that waits for one agent turn to end — the embed
//! widget's event stream and A2A's task stream. The answer is delivered
//! whole, so the stream has nothing to say until the turn's claim is
//! released; it re-reads the turn then, and otherwise only keeps the
//! connection alive.

use std::time::Duration;

use aiplane_runtime::agents::embed::ReleaseWatch;
use session_core::chat_json::SseTx;
use tokio::time::Instant;

/// A comment at this interval keeps proxies and clients from treating a
/// long-running turn as a dead connection.
const KEEPALIVE: Duration = Duration::from_secs(15);

/// How often the turn is re-read when no release woke the stream. A safety
/// net only, for a release that never comes (a claim leaked by a bug): the
/// release is what ends the wait.
const SAFETY_POLL: Duration = Duration::from_secs(30);

/// A stream ends after this long even if the turn still runs; the client
/// attaches again and gets a fresh snapshot.
const STREAM_LIMIT: Duration = Duration::from_secs(600);

pub(crate) enum Waited {
    /// Read the turn again: its claim was released, or the safety poll is due.
    Reread,
    /// The stream ran for its whole time; end it.
    Expired,
    /// The client went away.
    Gone,
}

pub(crate) struct TurnWait {
    releases: ReleaseWatch,
    started: Instant,
    last_sent: Instant,
    last_read: Instant,
}

impl TurnWait {
    /// `releases` must be taken before the turn is first read, so a release
    /// in between is not missed.
    pub(crate) fn new(releases: ReleaseWatch) -> Self {
        let now = Instant::now();
        Self {
            releases,
            started: now,
            last_sent: now,
            last_read: now,
        }
    }

    pub(crate) async fn next(&mut self, tx: &SseTx) -> Waited {
        loop {
            if self.started.elapsed() >= STREAM_LIMIT {
                return Waited::Expired;
            }
            let quiet = KEEPALIVE.saturating_sub(self.last_sent.elapsed());
            let released = match tokio::time::timeout(quiet, self.releases.changed()).await {
                Ok(Ok(())) => true,
                Ok(Err(_)) => {
                    tokio::time::sleep(quiet).await;
                    false
                }
                Err(_) => false,
            };
            if tx.is_closed() {
                return Waited::Gone;
            }
            if released || self.last_read.elapsed() >= SAFETY_POLL {
                self.last_read = Instant::now();
                return Waited::Reread;
            }
            if self.last_sent.elapsed() >= KEEPALIVE {
                let _ = tx.unbounded_send(Ok(rama::bytes::Bytes::from_static(b": working\n\n")));
                self.last_sent = Instant::now();
            }
        }
    }
}
