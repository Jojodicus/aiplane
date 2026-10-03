// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! A public agent's visitor rates (`docs/agents.md` §5, "What #92 built"):
//! at most so many requests per conversation and per client IP in a sliding
//! window, exact to the second, plus the same window arithmetic for a
//! verifier's sends. Separate from the spend limits in
//! `aiplane_core::server::limits`, which meter tokens per user, token and
//! agent and know nothing about visitors.

use jiff::{SignedDuration, Timestamp};

use crate::db::Pool;
use crate::db::inbound::{self, Inbound};

/// Gate a visitor's request to agent `who.principal_id` on its
/// per-conversation and per-IP rates. The counted events are the rows the
/// requests leave behind (conversations started, messages sent), so a
/// refused request, which writes nothing, never counts. Only the newest `max`
/// events of a window are read: whether there are `max` of them, and when the
/// oldest of those leaves, is all [`sliding_window`] decides on. Read errors
/// admit: like the spend limits, a rate read must not wedge live traffic.
pub async fn check_visitor(
    pool: &Pool,
    rates: &VisitorRates,
    who: &VisitorKey<'_>,
    now: Timestamp,
) -> Result<(), RateExceeded> {
    if let Some(conversation) = who.conversation {
        let since = rates.visitor.since(now);
        let times = inbound::message_times(pool, conversation, since, rates.visitor.max)
            .await
            .unwrap_or_else(|err| {
                tracing::warn!(error = %err, "rates: conversation message times; allowing");
                Vec::new()
            });
        sliding_window(RateScope::Visitor, rates.visitor, &times, now)?;
    }
    if let Some(ip) = who.ip {
        let since = rates.ip.since(now);
        let times = inbound::ip_event_times(pool, who.principal_id, ip, since, rates.ip.max)
            .await
            .unwrap_or_else(|err| {
                tracing::warn!(error = %err, "rates: per-IP event times; allowing");
                Vec::new()
            });
        sliding_window(RateScope::Ip, rates.ip, &times, now)?;
    }
    Ok(())
}

/// At most `max` events in any `per`. Unlike a limit's
/// [`Window`](aiplane_core::server::db::limits::Window), which snaps to the
/// hour because it meters spend, a rate is exact to the second: a visitor
/// told to wait 40 seconds may send again after 40 seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    pub max: u32,
    pub per: SignedDuration,
}

impl Rate {
    fn since(self, now: Timestamp) -> Timestamp {
        now.checked_sub(self.per).unwrap_or(now)
    }
}

/// An agent's visitor rates: per visitor session and per client IP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisitorRates {
    pub visitor: Rate,
    pub ip: Rate,
}

/// Whose events a visitor rate counts. No conversation yet when one is
/// being started; no IP when the request carries none.
#[derive(Debug, Clone, Copy)]
pub struct VisitorKey<'a> {
    pub principal_id: &'a str,
    /// The conversation asking, on whichever channel: each counts against
    /// `rates.visitor`.
    pub conversation: Option<Inbound<'a>>,
    pub ip: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateScope {
    Visitor,
    Ip,
    /// A verifier's sends to one email address, across conversations.
    Email,
    /// A verifier's sends or lookups in one conversation.
    Session,
}

impl RateScope {
    pub fn as_str(self) -> &'static str {
        match self {
            RateScope::Visitor => "visitor",
            RateScope::Ip => "ip",
            RateScope::Email => "email",
            RateScope::Session => "session",
        }
    }
}

/// A refused visitor request: `scope` already reached `max` in the last
/// `per`. `retry_after_secs` is when the oldest counted event leaves the
/// window, served as `Retry-After`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateExceeded {
    pub scope: RateScope,
    pub max: u32,
    pub per: SignedDuration,
    pub retry_after_secs: i64,
}

/// Whether one more event fits `rate` given the `events` already in its
/// window (any order; older ones are ignored).
pub fn sliding_window(
    scope: RateScope,
    rate: Rate,
    events: &[Timestamp],
    now: Timestamp,
) -> Result<(), RateExceeded> {
    let since = rate.since(now);
    let mut inside: Vec<Timestamp> = events.iter().copied().filter(|t| *t >= since).collect();
    if inside.len() < rate.max as usize {
        return Ok(());
    }
    inside.sort_unstable_by(|a, b| b.cmp(a));
    // A zero rate admits nothing, ever; no event's departure frees a slot, so
    // the honest retry is one window from now.
    let leaves = match (rate.max as usize).checked_sub(1) {
        Some(nth) => inside[nth].checked_add(rate.per).unwrap_or(now),
        None => now.checked_add(rate.per).unwrap_or(now),
    };
    Err(RateExceeded {
        scope,
        max: rate.max,
        per: rate.per,
        retry_after_secs: (leaves.as_second() - now.as_second()).max(1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    const TEN_MIN: SignedDuration = SignedDuration::from_secs(600);

    #[test]
    fn a_rate_admits_until_max_events_sit_inside_the_window() {
        let rate = Rate {
            max: 2,
            per: TEN_MIN,
        };
        let now = at("2026-10-01T12:10:00Z");
        let old = at("2026-10-01T11:59:00Z");
        let one = at("2026-10-01T12:02:00Z");
        let two = at("2026-10-01T12:05:00Z");
        assert!(sliding_window(RateScope::Visitor, rate, &[old, one], now).is_ok());
        let err = sliding_window(RateScope::Visitor, rate, &[two, old, one], now).unwrap_err();
        assert_eq!(err.scope, RateScope::Visitor);
        assert_eq!(err.max, 2);
        assert_eq!(
            err.retry_after_secs, 120,
            "12:02 leaves the window at 12:12, two minutes from now"
        );
    }

    #[test]
    fn a_zero_rate_refuses_without_panicking() {
        let rate = Rate {
            max: 0,
            per: TEN_MIN,
        };
        let now = at("2026-10-01T12:10:00Z");
        let err = sliding_window(RateScope::Visitor, rate, &[], now).unwrap_err();
        assert_eq!(err.max, 0);
        assert_eq!(
            err.retry_after_secs, 600,
            "nothing will ever fit; retry after a window"
        );
        assert!(sliding_window(RateScope::Ip, rate, &[at("2026-10-01T12:09:00Z")], now).is_err());
    }

    #[test]
    fn retry_after_is_when_enough_events_have_left_the_window() {
        let rate = Rate {
            max: 2,
            per: TEN_MIN,
        };
        let now = at("2026-10-01T12:10:00Z");
        let events = [
            at("2026-10-01T12:01:00Z"),
            at("2026-10-01T12:03:00Z"),
            at("2026-10-01T12:09:00Z"),
        ];
        let err = sliding_window(RateScope::Ip, rate, &events, now).unwrap_err();
        assert_eq!(
            err.retry_after_secs, 180,
            "below max once 12:03 is gone too, at 12:13"
        );
    }
}
