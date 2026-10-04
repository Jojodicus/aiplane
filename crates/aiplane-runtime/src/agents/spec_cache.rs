// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! Published agent versions, parsed and compiled once.
//!
//! A version is immutable once published, so its typed spec
//! ([`AgentSpec`]), state schema, route gates and output filter — the parts
//! that cost a JSON parse and a regex compile per slot `pattern` and filter —
//! are built the first time a run or a visitor admission needs them and
//! shared after that. Runtime code reads the spec only through these parts. Which version is
//! live is not immutable: [`SpecCache::live`] reads the pointer every time,
//! [`SpecCache::live_recent`] holds it for [`LIVE_TTL`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aiplane_agents::db::agents as agents_db;
use aiplane_core::server::db::{DbError, Pool};
use serde_json::Value;

use super::gate::RouteGates;
use super::output_filter::OutputFilter;
use super::spec::AgentSpec;
use super::state::StateSchema;

/// How long [`SpecCache::live_recent`] trusts the live pointer it read.
pub const LIVE_TTL: Duration = Duration::from_secs(5);

/// Compiled versions kept at most; the least recently used goes first.
const MAX_VERSIONS: usize = 256;

/// One version's spec and what is built from it. A spec that does not
/// compile keeps the reason, so every run of it fails the same way without
/// compiling it again.
#[derive(Debug)]
pub struct CompiledSpec {
    pub version: i64,
    parts: Result<SpecParts, String>,
}

#[derive(Debug, Clone)]
pub(crate) struct SpecParts {
    pub agent: Arc<AgentSpec>,
    pub schema: Arc<StateSchema>,
    pub gates: Arc<RouteGates>,
    /// Kept apart from the rest so a run reports the failures it checks
    /// first (`finish.schema`) before a broken filter.
    pub output_filter: Result<Option<OutputFilter>, String>,
}

impl CompiledSpec {
    pub fn parse(version: i64, text: &str) -> Self {
        match serde_json::from_str(text) {
            Ok(spec) => Self::compile(version, spec),
            Err(e) => Self {
                version,
                parts: Err(format!("it is not JSON ({e})")),
            },
        }
    }

    pub fn compile(version: i64, spec: Value) -> Self {
        Self {
            version,
            parts: build(&spec),
        }
    }

    /// The typed spec, or why the stored one does not read as one.
    pub fn agent(&self) -> Result<&AgentSpec, &str> {
        self.parts().map(|p| &*p.agent)
    }

    /// The built parts, or why the spec cannot run (phrased to follow "cannot
    /// run its live version N: ").
    pub(crate) fn parts(&self) -> Result<&SpecParts, &str> {
        self.parts.as_ref().map_err(String::as_str)
    }
}

fn build(spec: &Value) -> Result<SpecParts, String> {
    let agent = AgentSpec::from_value(spec)
        .map_err(|e| format!("it does not read as an agent spec ({e})"))?;
    let schema =
        StateSchema::from_spec(spec).map_err(|i| format!("at `{}`, {}", i.path, i.message))?;
    let gates =
        RouteGates::from_spec(&agent).map_err(|i| format!("at `{}`, {}", i.path, i.message))?;
    let output_filter =
        OutputFilter::from_spec(&agent).map_err(|e| format!("`publish.output_filter`: {e}"));
    Ok(SpecParts {
        agent: Arc::new(agent),
        schema: Arc::new(schema),
        gates: Arc::new(gates),
        output_filter,
    })
}

/// The process-wide cache of compiled versions, keyed by `(agent, version)`.
#[derive(Clone, Default)]
pub struct SpecCache {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    versions: HashMap<(String, i64), (u64, Arc<CompiledSpec>)>,
    live: HashMap<String, (Instant, Option<i64>)>,
    clock: u64,
}

impl Inner {
    fn get(&mut self, agent_id: &str, version: i64) -> Option<Arc<CompiledSpec>> {
        self.clock += 1;
        let clock = self.clock;
        self.versions
            .get_mut(&(agent_id.to_string(), version))
            .map(|(used, spec)| {
                *used = clock;
                spec.clone()
            })
    }

    fn insert(&mut self, agent_id: &str, spec: Arc<CompiledSpec>) {
        if self.versions.len() >= MAX_VERSIONS
            && let Some(oldest) = self
                .versions
                .iter()
                .min_by_key(|(_, (used, _))| *used)
                .map(|(key, _)| key.clone())
        {
            self.versions.remove(&oldest);
        }
        self.clock += 1;
        self.versions
            .insert((agent_id.to_string(), spec.version), (self.clock, spec));
    }
}

impl SpecCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Published version `version` of agent `agent_id`; `None` when it has
    /// no such version.
    pub async fn version(
        &self,
        db: &Pool,
        agent_id: &str,
        version: i64,
    ) -> Result<Option<Arc<CompiledSpec>>, DbError> {
        if let Some(hit) = self.lock().get(agent_id, version) {
            return Ok(Some(hit));
        }
        let Some(row) = agents_db::version(db, agent_id, version).await? else {
            return Ok(None);
        };
        let compiled = Arc::new(CompiledSpec::parse(row.version, &row.spec));
        self.lock().insert(agent_id, compiled.clone());
        Ok(Some(compiled))
    }

    /// The version agent `agent_id` points at right now; `None` when it was
    /// never published or does not exist.
    pub async fn live(
        &self,
        db: &Pool,
        agent_id: &str,
    ) -> Result<Option<Arc<CompiledSpec>>, DbError> {
        let live = agents_db::live_version(db, agent_id).await?;
        self.remember_live(agent_id, live);
        self.at(db, agent_id, live).await
    }

    /// [`Self::live`], trusting a pointer read within [`LIVE_TTL`]: for a
    /// path a flood of requests reaches, such as a visitor admission.
    pub async fn live_recent(
        &self,
        db: &Pool,
        agent_id: &str,
    ) -> Result<Option<Arc<CompiledSpec>>, DbError> {
        let recent = self
            .lock()
            .live
            .get(agent_id)
            .filter(|(at, _)| at.elapsed() < LIVE_TTL)
            .map(|(_, version)| *version);
        match recent {
            Some(live) => self.at(db, agent_id, live).await,
            None => self.live(db, agent_id).await,
        }
    }

    async fn at(
        &self,
        db: &Pool,
        agent_id: &str,
        version: Option<i64>,
    ) -> Result<Option<Arc<CompiledSpec>>, DbError> {
        match version {
            Some(v) => self.version(db, agent_id, v).await,
            None => Ok(None),
        }
    }

    fn remember_live(&self, agent_id: &str, version: Option<i64>) {
        let mut inner = self.lock();
        inner.live.retain(|_, (at, _)| at.elapsed() < LIVE_TTL);
        inner
            .live
            .insert(agent_id.to_string(), (Instant::now(), version));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiplane_agents::db::system_principals as sp;
    use serde_json::json;

    async fn db_with_agent() -> (Pool, String) {
        let db = aiplane_core::server::db::open(std::path::Path::new(":memory:"))
            .await
            .unwrap();
        let now = jiff::Timestamp::now();
        aiplane_core::server::db::users::upsert(
            &db,
            &aiplane_core::server::db::users::User {
                id: "u1".into(),
                email: "owner@example.com".into(),
                name: None,
                roles: vec![],
                created_at: now,
                updated_at: now,
                timezone: None,
                speech_voice: None,
            },
        )
        .await
        .unwrap();
        let row = agents_db::create(
            &db,
            &sp::NewPrincipal {
                name: "support",
                display: "support",
                description: "",
            },
            "{}",
            "u1",
        )
        .await
        .unwrap()
        .unwrap();
        (db, row.principal.id)
    }

    fn spec(model: &str) -> String {
        json!({ "main": { "model": model } }).to_string()
    }

    #[tokio::test]
    async fn a_version_is_compiled_once_and_shared_by_every_reader() {
        let (db, id) = db_with_agent().await;
        agents_db::publish(&db, &id, &spec("chat"), "u1")
            .await
            .unwrap();
        let cache = SpecCache::default();
        let first = cache.live(&db, &id).await.unwrap().expect("published");
        let pinned = cache.version(&db, &id, 1).await.unwrap().unwrap();
        let recent = cache.live_recent(&db, &id).await.unwrap().unwrap();
        assert!(Arc::ptr_eq(&first, &pinned) && Arc::ptr_eq(&first, &recent));
        assert_eq!(first.agent().unwrap().main_model(), Some("chat"));
        assert!(first.parts().is_ok());
        assert!(cache.version(&db, &id, 9).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn the_live_pointer_is_read_fresh_and_held_only_by_the_recent_read() {
        let (db, id) = db_with_agent().await;
        let cache = SpecCache::default();
        assert!(cache.live(&db, &id).await.unwrap().is_none(), "unpublished");
        agents_db::publish(&db, &id, &spec("one"), "u1")
            .await
            .unwrap();
        assert!(
            cache.live_recent(&db, &id).await.unwrap().is_none(),
            "the recent read still holds the unpublished pointer"
        );
        assert_eq!(cache.live(&db, &id).await.unwrap().unwrap().version, 1);
        agents_db::publish(&db, &id, &spec("two"), "u1")
            .await
            .unwrap();
        assert_eq!(
            cache.live_recent(&db, &id).await.unwrap().unwrap().version,
            1
        );
        assert_eq!(cache.live(&db, &id).await.unwrap().unwrap().version, 2);
    }

    #[test]
    fn a_spec_that_does_not_compile_keeps_the_reason() {
        let text = CompiledSpec::parse(3, "{not json");
        assert!(text.parts().unwrap_err().starts_with("it is not JSON"));
        let renamed = CompiledSpec::compile(3, json!({ "main": { "pol": "chat" } }));
        assert!(
            renamed
                .agent()
                .unwrap_err()
                .starts_with("it does not read as an agent spec"),
            "a renamed field fails loudly instead of falling back to a default"
        );
        let filter = CompiledSpec::compile(
            3,
            json!({ "publish": { "output_filter": { "patterns": { "id": "(" } } } }),
        );
        assert!(
            filter.parts().unwrap().output_filter.is_err(),
            "a broken filter does not hide the parts that built"
        );
    }

    #[test]
    fn the_cache_holds_a_bounded_number_of_versions_dropping_the_least_used() {
        let mut inner = Inner::default();
        for v in 0..MAX_VERSIONS as i64 {
            inner.insert("a", Arc::new(CompiledSpec::compile(v, Value::Null)));
        }
        assert!(inner.get("a", 0).is_some(), "touching version 0 keeps it");
        inner.insert("a", Arc::new(CompiledSpec::compile(-1, Value::Null)));
        assert_eq!(inner.versions.len(), MAX_VERSIONS);
        assert!(inner.get("a", 0).is_some());
        assert!(inner.get("a", 1).is_none(), "the least recently used went");
        assert!(inner.get("a", -1).is_some());
    }
}
