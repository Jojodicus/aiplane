// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Checking an agent's hash chains (`docs/agents.md` → "Anchors",
//! "Verification watermarks").
//!
//! **Watermarks.** A chain that checked out is remembered in
//! `activity_verified`: its `seq` and `hash` there (and, for the agent's own
//! chain, the anchors it had read), signed under the log key like an event.
//! [`verify`] resumes each chain from its watermark, so a check costs what
//! was written since the last one; [`verify_full`] walks every chain from its
//! start. A watermark the database alone could have moved — unsigned, under
//! a key the ring lacks, not matching its signature — or whose event is gone
//! or changed is ignored, and the chain walked whole.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use serde_json::{Value, json};
use sqlx::Row;

use super::super::{DbError, Pool};
use super::{
    AuditKind, COLUMNS, StoredEvent, UNKEYED, agent_chain, canonical_json, key_ring, sign_with,
    signing_key_id, stored,
};
use aiplane_core::server::crypto::ActivityKey;

/// The first link of a chain that does not hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BrokenLink {
    pub chain_key: String,
    /// The `seq` the walk expected next.
    pub seq: i64,
    pub event_id: Option<String>,
    pub reason: String,
}

/// A chain's newest event, for an operator to keep outside the gateway.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChainHead {
    pub chain_key: String,
    pub seq: i64,
    pub hash: Option<String>,
}

/// What [`verify`] found for one agent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Verification {
    pub chains: u64,
    /// Every event the chains hold that is now known to be sound, whether
    /// this check or an earlier one hashed it.
    pub events: u64,
    /// The events this check hashed: those written since each chain's
    /// watermark, or all of them on a full walk.
    pub checked: u64,
    /// Conversation events newer than their chain's latest anchor: written
    /// after the last turn ended (or by a turn whose anchor failed), so a
    /// removal of them would not show yet.
    pub unanchored: u64,
    /// The agent's own chain's newest event. Its `seq` and `hash` pin every
    /// anchor, so keeping them outside the gateway pins the whole log.
    pub head: Option<ChainHead>,
    pub broken: Option<BrokenLink>,
}

impl Verification {
    pub fn ok(&self) -> bool {
        self.broken.is_none()
    }
}

/// How much of each chain a check walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Depth {
    FromWatermark,
    Full,
}

/// Rows read per query during a walk, so a long chain is walked in bounded
/// memory.
const VERIFY_BATCH: i64 = 256;

/// The newest checkpoint of a chain: where its stored part begins.
const LATEST_CHECKPOINT_SQL: &str = "SELECT detail FROM agent_audit
                                      WHERE chain_key = ? AND kind = 'chain_checkpoint'
                                      ORDER BY seq DESC LIMIT 1";

/// A conversation chain's anchored head: `seq` and `hash`.
type Anchor = (i64, Option<String>);

/// Where a walk of one chain ended.
struct Walked {
    last_seq: i64,
    last_hash: Option<String>,
}

/// Where a walk starts: after `seq`, whose hash is `hash` — `(0, None)` for a
/// chain that was never cut.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Base {
    seq: i64,
    hash: Option<String>,
}

/// Check every chain of `agent_id` — its own first, then each conversation —
/// from where the last check left it: `seq` counts up without a gap,
/// `prev_hash` is the hash of the event before, and `hash` is the HMAC of the
/// event as stored under the key it names. Then check each conversation
/// chain against its latest anchor in the agent's chain: it must still reach
/// the anchored `seq` with the anchored `hash`, and an anchored chain must
/// exist unless the retention sweep recorded removing it. Stops at the first
/// thing that does not hold.
///
/// What it cannot show: a change below a chain's watermark — [`verify_full`]
/// re-walks everything —, events removed from the tail of the agent's own
/// chain together with the conversation tails they anchored — compare
/// [`Verification::head`] with a copy kept elsewhere for that — nor anything
/// forged by someone holding the gateway's at-rest key.
pub async fn verify(pool: &Pool, agent_id: &str) -> Result<Verification, DbError> {
    verify_with(pool, agent_id, Depth::FromWatermark).await
}

/// [`verify`], walking every chain from its start whatever its watermark.
pub async fn verify_full(pool: &Pool, agent_id: &str) -> Result<Verification, DbError> {
    verify_with(pool, agent_id, Depth::Full).await
}

async fn verify_with(pool: &Pool, agent_id: &str, depth: Depth) -> Result<Verification, DbError> {
    let ring = key_ring();
    let mut out = Verification::default();
    if let Some(broken) = event_outside_every_chain(pool, agent_id).await? {
        out.broken = Some(broken);
        return Ok(out);
    }
    let Some(AnchorBook { mut anchors, swept }) =
        verify_agent_chain(pool, agent_id, &ring, depth, &mut out).await?
    else {
        return Ok(out);
    };
    for key in conversation_chains(pool, agent_id).await? {
        let anchor = anchors.remove(&key).filter(|_| !swept.contains(&key));
        if !verify_conversation(pool, &key, anchor, &ring, depth, &mut out).await? {
            return Ok(out);
        }
    }
    out.broken = first_missing_anchor(anchors, &swept);
    Ok(out)
}

/// Every event the gateway writes has a chain, so one without was inserted
/// behind its back.
async fn event_outside_every_chain(
    pool: &Pool,
    agent_id: &str,
) -> Result<Option<BrokenLink>, DbError> {
    let id: Option<String> = sqlx::query_scalar(
        "SELECT id FROM agent_audit WHERE agent_id = ? AND chain_key IS NULL LIMIT 1",
    )
    .bind(agent_id)
    .fetch_optional(pool)
    .await?;
    Ok(id.map(|id| BrokenLink {
        chain_key: String::new(),
        seq: 0,
        event_id: Some(id),
        reason: "it belongs to no chain: it was inserted outside the gateway".into(),
    }))
}

async fn conversation_chains(pool: &Pool, agent_id: &str) -> Result<Vec<String>, DbError> {
    Ok(sqlx::query_scalar(
        "SELECT DISTINCT chain_key FROM agent_audit
          WHERE agent_id = ? AND chain_key IS NOT NULL AND chain_key != ?
          ORDER BY chain_key",
    )
    .bind(agent_id)
    .bind(agent_chain(agent_id))
    .fetch_all(pool)
    .await?)
}

/// Walk the agent's own chain from its newest checkpoint (or its
/// watermark), reading its anchors and sweep markers as it goes. `None` when
/// a link does not hold; `out.broken` then says which.
async fn verify_agent_chain(
    pool: &Pool,
    agent_id: &str,
    ring: &[ActivityKey],
    depth: Depth,
    out: &mut Verification,
) -> Result<Option<AnchorBook>, DbError> {
    let own = agent_chain(agent_id);
    let checkpoint: Option<String> = sqlx::query_scalar(LATEST_CHECKPOINT_SQL)
        .bind(&own)
        .fetch_optional(pool)
        .await?;
    let checkpoint: Value = checkpoint
        .and_then(|d| serde_json::from_str(&d).ok())
        .unwrap_or(Value::Null);
    let origin = Base {
        seq: checkpoint["base_seq"].as_i64().unwrap_or(0),
        hash: checkpoint["base_hash"].as_str().map(str::to_string),
    };
    let (base, book) = start_of(pool, &own, &origin, ring, depth).await?;
    let mut book = match book {
        Some(book) => AnchorBook::from_json(&book),
        None => {
            let mut book = AnchorBook::default();
            book.carry(&checkpoint);
            book
        }
    };
    let Some(walked) = walk(pool, &own, base.clone(), ring, out, |e| book.read(e)).await? else {
        return Ok(None);
    };
    if walked.last_seq == origin.seq {
        out.chains -= 1;
        return Ok(Some(book));
    }
    out.events += (walked.last_seq - origin.seq).max(0) as u64;
    if walked.last_seq > base.seq {
        Watermark {
            seq: walked.last_seq,
            hash: walked.last_hash.clone(),
            book: Some(book.to_json()),
        }
        .save(pool, &own, ring)
        .await?;
    }
    out.head = Some(ChainHead {
        chain_key: own,
        seq: walked.last_seq,
        hash: walked.last_hash,
    });
    Ok(Some(book))
}

/// Walk conversation chain `key` from its watermark and check it against
/// its `anchor`. `false` when something does not hold; `out.broken` then
/// says what.
async fn verify_conversation(
    pool: &Pool,
    key: &str,
    anchor: Option<Anchor>,
    ring: &[ActivityKey],
    depth: Depth,
    out: &mut Verification,
) -> Result<bool, DbError> {
    let (base, _) = start_of(pool, key, &Base::default(), ring, depth).await?;
    let Some(walked) = walk(pool, key, base.clone(), ring, out, |_| {}).await? else {
        return Ok(false);
    };
    out.events += walked.last_seq.max(0) as u64;
    let anchored_seq = anchor.as_ref().map_or(0, |(seq, _)| *seq);
    out.unanchored += (walked.last_seq - anchored_seq).max(0) as u64;
    if let Some(anchor) = anchor
        && let Some(broken) = check_anchor(pool, key, walked.last_seq, anchor).await?
    {
        out.broken = Some(broken);
        return Ok(false);
    }
    if walked.last_seq > base.seq {
        Watermark {
            seq: walked.last_seq,
            hash: walked.last_hash,
            book: None,
        }
        .save(pool, key, ring)
        .await?;
    }
    Ok(true)
}

/// Where a walk of `key` starts: its watermark — with the anchor book it
/// carries — when `depth` allows one, it is at or past `origin` and its event
/// is still the one that was verified; else `origin`, so a chain cut or
/// rewritten at its watermark is walked whole and the walk names what broke.
async fn start_of(
    pool: &Pool,
    key: &str,
    origin: &Base,
    ring: &[ActivityKey],
    depth: Depth,
) -> Result<(Base, Option<Value>), DbError> {
    let mark = match depth {
        Depth::Full => None,
        Depth::FromWatermark => Watermark::load(pool, key, ring).await?,
    };
    let Some(mark) = mark.filter(|m| m.seq >= origin.seq) else {
        return Ok((origin.clone(), None));
    };
    let held = if mark.seq == origin.seq {
        Some(origin.hash.clone())
    } else {
        hash_at(pool, key, mark.seq).await?
    };
    if held != Some(mark.hash.clone()) {
        return Ok((origin.clone(), None));
    }
    Ok((
        Base {
            seq: mark.seq,
            hash: mark.hash,
        },
        mark.book,
    ))
}

/// The stored hash of event `seq` of chain `key`; `None` when it is gone.
/// From `idx_agent_audit_chain_head` alone.
async fn hash_at(pool: &Pool, key: &str, seq: i64) -> Result<Option<Option<String>>, DbError> {
    Ok(
        sqlx::query_scalar("SELECT hash FROM agent_audit WHERE chain_key = ? AND seq = ?")
            .bind(key)
            .bind(seq)
            .fetch_optional(pool)
            .await?,
    )
}

/// A conversation chain that ends at `last_seq` against its latest anchor:
/// it must reach the anchored event, and that event must be the one that was
/// anchored.
async fn check_anchor(
    pool: &Pool,
    key: &str,
    last_seq: i64,
    (seq, hash): Anchor,
) -> Result<Option<BrokenLink>, DbError> {
    let reason = if last_seq < seq {
        format!(
            "the chain ends at event {last_seq} but was anchored at event {seq}: its newest \
             events were removed"
        )
    } else if hash_at(pool, key, seq).await? != Some(hash) {
        format!(
            "event {seq} is not the event that was anchored: the chain was rewritten from there"
        )
    } else {
        return Ok(None);
    };
    Ok(Some(BrokenLink {
        chain_key: key.to_string(),
        seq,
        event_id: None,
        reason,
    }))
}

/// The first anchored conversation chain no walk met and no sweep removed.
fn first_missing_anchor(
    anchors: HashMap<String, Anchor>,
    swept: &HashSet<String>,
) -> Option<BrokenLink> {
    let (key, seq) = anchors
        .into_iter()
        .filter(|(key, _)| !swept.contains(key))
        .map(|(key, (seq, _))| (key, seq))
        .min()?;
    Some(BrokenLink {
        chain_key: key,
        seq,
        event_id: None,
        reason: format!(
            "the chain is gone, though it was anchored at event {seq} and no retention sweep \
             removed it"
        ),
    })
}

/// Walk chain `key` link by link from `base`, handing every event to `on`.
/// `None` when a link does not hold; `out.broken` then says which.
async fn walk(
    pool: &Pool,
    key: &str,
    base: Base,
    ring: &[ActivityKey],
    out: &mut Verification,
    mut on: impl FnMut(&StoredEvent),
) -> Result<Option<Walked>, DbError> {
    out.chains += 1;
    let mut expected = base.seq + 1;
    let mut prev: Option<String> = base.hash;
    let sql = format!(
        "SELECT {COLUMNS} FROM agent_audit WHERE chain_key = ? AND seq >= ?
          ORDER BY seq LIMIT ?"
    );
    loop {
        let rows = sqlx::query(&sql)
            .bind(key)
            .bind(expected)
            .bind(VERIFY_BATCH)
            .fetch_all(pool)
            .await?;
        let done = (rows.len() as i64) < VERIFY_BATCH;
        for row in &rows {
            let event = stored(row)?;
            out.checked += 1;
            if let Some(reason) = link_fault(&event, expected, prev.as_deref(), ring) {
                out.broken = Some(BrokenLink {
                    chain_key: key.to_string(),
                    seq: expected,
                    event_id: Some(event.id.clone()),
                    reason,
                });
                return Ok(None);
            }
            on(&event);
            prev = event.hash.clone();
            expected += 1;
        }
        if done {
            break;
        }
    }
    Ok(Some(Walked {
        last_seq: expected - 1,
        last_hash: prev,
    }))
}

/// Why `event` is not the sound link `expected` after `prev`, if it is not.
fn link_fault(
    event: &StoredEvent,
    expected: i64,
    prev: Option<&str>,
    ring: &[ActivityKey],
) -> Option<String> {
    if event.seq != Some(expected) {
        return Some(format!(
            "event {expected} of the chain is missing: the next one stored is {}",
            event.seq.unwrap_or_default()
        ));
    }
    if event.prev_hash.as_deref() != prev {
        return Some("its prev_hash is not the hash of the event before it".into());
    }
    if matches!(event.key_id.as_deref(), None | Some(UNKEYED)) && !ring.is_empty() {
        return Some(
            "it is not signed with the gateway's log key: it was written or rewritten outside \
             the gateway"
                .into(),
        );
    }
    match event.expected_hash(ring) {
        Some(hash) if event.hash.as_deref() == Some(hash.as_str()) => None,
        Some(_) => {
            Some("its content does not match its hash: it was changed after it was written".into())
        }
        None => Some(format!(
            "it is signed with log key {}, which this gateway does not hold: the at-rest key \
             was replaced without keeping the old one, or the event was forged",
            event.key_id.as_deref().unwrap_or_default()
        )),
    }
}

/// How far a chain was found sound, signed so the database alone cannot
/// move it forward over a change.
struct Watermark {
    seq: i64,
    hash: Option<String>,
    /// The agent chain's [`AnchorBook`] as of `seq`; `None` for a
    /// conversation chain.
    book: Option<Value>,
}

impl Watermark {
    fn canonical(&self, key: &str) -> String {
        canonical_json(&json!({
            "book": self.book,
            "chain_key": key,
            "hash": self.hash,
            "seq": self.seq,
        }))
    }

    /// `key`'s watermark, if it has one this gateway signed.
    async fn load(pool: &Pool, key: &str, ring: &[ActivityKey]) -> Result<Option<Self>, DbError> {
        let Some(row) = sqlx::query(
            "SELECT seq, hash, book, key_id, mac FROM activity_verified WHERE chain_key = ?",
        )
        .bind(key)
        .fetch_optional(pool)
        .await?
        else {
            return Ok(None);
        };
        let key_id: String = row.try_get("key_id")?;
        let mac: String = row.try_get("mac")?;
        if key_id == UNKEYED && !ring.is_empty() {
            return Ok(None);
        }
        let book: Option<String> = row.try_get("book")?;
        let mark = Self {
            seq: row.try_get("seq")?,
            hash: row.try_get("hash")?,
            book: book.and_then(|b| serde_json::from_str(&b).ok()),
        };
        let signed = sign_with(ring, Some(&key_id), &mark.canonical(key));
        Ok((signed.as_deref() == Some(mac.as_str())).then_some(mark))
    }

    async fn save(&self, pool: &Pool, key: &str, ring: &[ActivityKey]) -> Result<(), DbError> {
        let key_id = signing_key_id(ring);
        let mac = sign_with(ring, Some(key_id), &self.canonical(key))
            .expect("the signing key is the ring's own");
        sqlx::query(
            "INSERT INTO activity_verified (chain_key, seq, hash, book, key_id, mac, verified_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (chain_key) DO UPDATE SET
                 seq = excluded.seq, hash = excluded.hash, book = excluded.book,
                 key_id = excluded.key_id, mac = excluded.mac,
                 verified_at = excluded.verified_at",
        )
        .bind(key)
        .bind(self.seq)
        .bind(&self.hash)
        .bind(self.book.as_ref().map(Value::to_string))
        .bind(key_id)
        .bind(mac)
        .bind(jiff::Timestamp::now().to_string())
        .execute(pool)
        .await?;
        Ok(())
    }
}

/// What an agent's own chain says about its conversation chains, read in
/// chain order: the latest anchor of each and which ones the sweep removed.
/// [`verify`] reads the stored chain into one; a cut reads the prefix it
/// removes, and its checkpoint carries the anchors that still matter.
#[derive(Default)]
pub(super) struct AnchorBook {
    anchors: HashMap<String, Anchor>,
    swept: HashSet<String>,
}

impl AnchorBook {
    pub(super) fn read(&mut self, e: &StoredEvent) {
        let detail: Value = serde_json::from_str(&e.detail).unwrap_or(Value::Null);
        match AuditKind::parse(&e.kind) {
            Some(AuditKind::ChainAnchored) => {
                if let Some(chain) = detail["chain_key"].as_str() {
                    self.anchors.insert(
                        chain.to_string(),
                        (
                            detail["seq"].as_i64().unwrap_or(0),
                            detail["hash"].as_str().map(str::to_string),
                        ),
                    );
                }
            }
            Some(AuditKind::ActivitySwept) => {
                if let Some(chain) = detail["chain_key"].as_str() {
                    self.swept.insert(chain.to_string());
                }
            }
            Some(AuditKind::ChainCheckpoint) => self.carry(&detail),
            _ => {}
        }
    }

    /// The anchors a checkpoint carried. They are older than any anchor the
    /// chain holds after the cut they were carried over, so they never
    /// replace one already read.
    fn carry(&mut self, checkpoint: &Value) {
        let Some(carried) = checkpoint["anchors"].as_object() else {
            return;
        };
        for (chain, anchor) in carried {
            self.anchors.entry(chain.clone()).or_insert((
                anchor["seq"].as_i64().unwrap_or(0),
                anchor["hash"].as_str().map(str::to_string),
            ));
        }
    }

    fn anchors_json(&self, keep: impl Fn(&str) -> bool) -> Value {
        let mut anchors: Vec<_> = self
            .anchors
            .iter()
            .filter(|(chain, _)| keep(chain))
            .collect();
        anchors.sort();
        Value::Object(
            anchors
                .into_iter()
                .map(|(chain, (seq, hash))| (chain.clone(), json!({ "seq": seq, "hash": hash })))
                .collect(),
        )
    }

    /// The anchors of chains not swept, as a checkpoint carries them.
    pub(super) fn still_guarding(&self) -> Value {
        self.anchors_json(|chain| !self.swept.contains(chain))
    }

    /// The whole book, as a watermark keeps it.
    fn to_json(&self) -> Value {
        let mut swept: Vec<&String> = self.swept.iter().collect();
        swept.sort();
        json!({ "anchors": self.anchors_json(|_| true), "swept": swept })
    }

    fn from_json(v: &Value) -> Self {
        let mut book = Self::default();
        book.carry(v);
        book.swept = v["swept"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s.as_str().map(str::to_string))
            .collect();
        book
    }
}
