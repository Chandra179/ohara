//! Worker loop (§6): claim jobs, dispatch stages, classify failures, record every
//! outcome in `stage_events`. Single worker by default — honest for an embedded
//! tool (§6); the lease protocol makes multi-worker safe when needed.

mod chunk;
mod clean;
mod embed;
mod execution;
mod extract;
mod extraction_contract;
mod query;
mod recovery;
mod retrieve;
mod runtime;
mod scrape;
mod synthesis;

use std::sync::{Arc, Mutex, MutexGuard};

use crate::Class;
use crate::config::Config;
use crate::control::{self, ControlDb, DbError, Stage};
use crate::engine::Fetcher;
use crate::knowledge::KnowledgeStore;
use crate::llm::Llm;

pub use crate::engine::HttpFetcher;
#[cfg(feature = "ladybug")]
pub use crate::knowledge::LadybugStore;
pub use chunk::{Chunk, chunk_document};
pub use clean::{CleanOutcome, ExtractError, ExtractedArticle, Extractor, ReadabilityExtractor};
#[cfg(feature = "onnx-embedder")]
pub use embed::LocalEmbedder;
pub use embed::{EmbedError, Embedder};
pub use query::{QueryError, QueryResponse, answer, query};
#[cfg(feature = "onnx-embedder")]
pub use retrieve::LocalReranker;
pub use retrieve::{
    IdentityReranker, Lang, QueryEntity, QueryEntitySource, QueryNormalizer, RerankError, Reranker,
    RetrieveError, RetrievedContext, Retriever, ScoredChunk, WhatlangNormalizer,
    fts_match_expression,
};

/// What a stage body reports on success (§10: domain outcomes are values, not
/// errors — they never route through [`StageError`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageOutcome {
    /// The stage advanced the pipeline: the milestone is set and the next stage's
    /// job is chained in the completion transaction (§6).
    Advance,
    /// The stage completed with a terminal domain outcome it recorded itself —
    /// a §8 Stage 2 quality rejection (`FAILED_QUALITY`) or a duplicate skip:
    /// the job is `DONE`, nothing chains, the milestone is not advanced.
    Stop,
}

/// Stage failures (§10): the only layer that decides what an error means for this
/// job. Domain outcomes (duplicate, low quality) are *values* in stage signatures —
/// they never appear here.
#[derive(Debug, thiserror::Error)]
pub enum StageError {
    /// Transient failure → backoff → `PENDING` (§6). `class` refines the retry
    /// policy per the §10 mapping.
    #[error("transient failure (attempt {attempt}): {source}")]
    Transient {
        /// The port error that caused this.
        source: Box<dyn std::error::Error + Send + Sync>,
        /// The retry class of `source`.
        class: Class,
        /// The ended-execution count for this run (1-based).
        attempt: u32,
    },
    /// Permanent failure → `DEAD` (§6).
    #[error("permanent failure: {reason}")]
    Permanent {
        /// Why this can never succeed.
        reason: String,
    },
    /// Fatal → stop scheduling, drain, reconcile at next boot (§6, §10).
    #[error("fatal: {source}")]
    Fatal {
        /// The underlying failure.
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

impl StageError {
    /// The classification driving the job state machine (§6).
    #[must_use]
    pub fn class(&self) -> Class {
        match self {
            StageError::Transient { .. } => Class::Retry,
            StageError::Permanent { .. } => Class::Permanent,
            StageError::Fatal { .. } => Class::Fatal,
        }
    }

    /// Wraps a port error as transient.
    pub fn transient(
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
        class: Class,
        attempt: u32,
    ) -> Self {
        StageError::Transient {
            source: source.into(),
            class,
            attempt,
        }
    }

    /// Builds a permanent failure.
    pub fn permanent(reason: impl Into<String>) -> Self {
        StageError::Permanent {
            reason: reason.into(),
        }
    }

    /// Wraps an error as fatal.
    pub fn fatal(source: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        StageError::Fatal {
            source: source.into(),
        }
    }
}

impl From<DbError> for StageError {
    /// A store failure while a stage runs is §10's "store returns corrupt data"
    /// shape: audit-preserving shutdown (drain, reconcile at next boot) beats
    /// pretending the stage can continue.
    fn from(source: DbError) -> Self {
        StageError::Fatal {
            source: Box::new(source),
        }
    }
}

/// The worker: owns the control database, the validated configuration, and the
/// ports. The database sits behind a [`Mutex`] because the embedded control
/// implementation serializes `SQLite` writes internally.
pub struct Worker {
    config: Arc<Config>,
    conn: Mutex<ControlDb>,
    handle: tokio::runtime::Handle,
    ports: runtime::WorkerPorts,
    id: String,
    _runtime_lock: crate::ops::RuntimeLock,
}

impl Worker {
    /// Boots a worker with the real ports: engine-plane fetcher (ladder leg 1),
    /// the readability extractor, the pinned local embedder, and the embedded
    /// `LadybugDB` knowledge store. Callers create the data directories first
    /// ([`run`] does).
    ///
    /// # Errors
    /// [`crate::BootError`] if the store cannot be opened/migrated, the runtime
    /// handle is unavailable, or any default port cannot be built.
    pub fn new(config: Arc<Config>) -> Result<Self, crate::BootError> {
        let runtime_lock = runtime::acquire_lock(&config)?;
        let ports = runtime::worker_ports(&config)?;
        Self::with_ports_locked(config, ports, runtime_lock)
    }

    /// Boots a worker with explicit ports (§14 integration: canned fetcher, fake
    /// extractor; §9: remote providers). Must be called inside a tokio runtime —
    /// stage bodies drive async port calls from blocking threads via its handle.
    ///
    /// # Errors
    /// [`crate::BootError`] if a provider violates the configured model
    /// contract, the store cannot be opened or migrated, or if no tokio runtime
    /// is active.
    pub fn with_ports(
        config: Arc<Config>,
        fetcher: Arc<dyn Fetcher>,
        extractor: Arc<dyn Extractor>,
        embedder: Arc<dyn Embedder>,
        knowledge: Arc<dyn KnowledgeStore>,
        llm: Arc<dyn Llm>,
    ) -> Result<Self, crate::BootError> {
        let runtime_lock = runtime::acquire_lock(&config)?;
        let ports = runtime::WorkerPorts {
            fetcher,
            extractor,
            embedder,
            knowledge,
            llm,
        };
        Self::with_ports_locked(config, ports, runtime_lock)
    }

    fn with_ports_locked(
        config: Arc<Config>,
        ports: runtime::WorkerPorts,
        runtime_lock: crate::ops::RuntimeLock,
    ) -> Result<Self, crate::BootError> {
        runtime::validate_embedder(&config, ports.embedder.as_ref())?;
        let id = format!("worker-{}", std::process::id());
        let conn = control::connect(config.db_path())?;
        let handle = tokio::runtime::Handle::try_current().map_err(|_| {
            crate::BootError::Worker("ohara must run inside a tokio runtime".to_string())
        })?;
        Ok(Self {
            config,
            conn: Mutex::new(conn),
            handle,
            ports,
            id,
            _runtime_lock: runtime_lock,
        })
    }

    /// Locks the control connection. A poisoned lock (a panic while a store call
    /// was in flight) is recovered: the store itself is transactional, so the
    /// next statement runs against a consistent state (§1.2.4 idempotency).
    fn conn(&self) -> MutexGuard<'_, ControlDb> {
        self.conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Runs the full §7.3 boot reconciliation sweep before the loop claims
    /// anything: interrupted §7.6 deletions are removed from `LadybugDB` first,
    /// then from `SQLite`, and the §5 audit trail is pruned to retention. Expired
    /// leases are deliberately not swept — the §6 claim reclaims them with the
    /// correct accounting.
    ///
    /// # Errors
    /// [`crate::BootError`] on control- or knowledge-plane failure.
    pub async fn reconcile(&self) -> Result<control::ReconcileReport, crate::BootError> {
        recovery::Reconciler::new(
            &self.conn,
            self.ports.knowledge.as_ref(),
            self.config.stage_events_retention(),
        )
        .run()
        .await
    }

    /// One scheduling step: claim the next runnable job in pipeline order,
    /// execute it, record the outcome. One job per call — the run loop spins
    /// back immediately while the queue drains, and tests/hosts can advance the
    /// pipeline step by step ([`Worker::tick_once`]).
    ///
    /// # Errors
    /// [`DbError`] on store failure; a `Fatal` stage error aborts scheduling (§6)
    /// and is returned as the tick's internal execution flow.
    fn tick(&self) -> Result<(usize, execution::Flow), DbError> {
        let conn = self.conn();
        let now = control::now();
        control::schedule_due_recrawls(&conn, &now)?;
        for stage in Stage::ALL {
            let Some(job) = control::claim_next(
                &conn,
                stage,
                &self.id,
                &now,
                self.config.lease_ttl().as_secs(),
            )?
            else {
                continue;
            };
            let executor =
                execution::StageExecutor::new(&self.config, &conn, &self.handle, &self.ports);
            let flow = executor.execute(stage, &job)?;
            return Ok((1, flow));
        }
        Ok((0, execution::Flow::Continue))
    }

    /// Advances the loop by one claim → execute → record step (§6). Public for
    /// embedding and §14 integration tests; [`run`] loops it until `Ctrl-C`.
    ///
    /// # Errors
    /// [`crate::BootError::Fatal`] if a stage failed fatally (the caller drains
    /// and reconciles at next boot); [`crate::BootError::Worker`] on store
    /// failure.
    pub fn tick_once(&self) -> Result<usize, crate::BootError> {
        match self.tick().map_err(crate::BootError::Control)? {
            (_, execution::Flow::Abort(err)) => Err(crate::BootError::Fatal(err)),
            (executed, execution::Flow::Continue) => Ok(executed),
        }
    }
}

/// In-crate test fakes for the pipeline ports (§14: fakes live next to the
/// ports they fake; §10: tests panic/unwrap freely). Used by the stage-body
/// unit tests in this module's children.
/// Runs the worker loop until interrupted (§6: single worker by default).
///
/// Boot: materializes the data directories, opens and migrates the control store,
/// runs the §7.3 reconciliation sweep, then loops claim → execute → record until
/// `Ctrl-C`.
///
/// # Errors
/// [`crate::BootError`] if directories or the store cannot be created, or on a
/// `Fatal` stage error after draining (§6).
pub async fn run(config: Config) -> Result<(), crate::BootError> {
    let config = Arc::new(config);
    tokio::fs::create_dir_all(config.data_dir()).await?;
    if let Some(parent) = config.db_path().parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let worker = Arc::new(Worker::new(Arc::clone(&config))?);
    worker.reconcile().await?;

    loop {
        let executed = tokio::task::spawn_blocking({
            let worker = Arc::clone(&worker);
            move || worker.tick_once()
        })
        .await
        .map_err(|join| crate::BootError::Worker(join.to_string()))??;

        if executed == 0 {
            tokio::select! {
                () = tokio::time::sleep(config.poll_interval()) => {}
                _ = tokio::signal::ctrl_c() => break,
            }
        }
        // Jobs executed: loop immediately to drain the queue.
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_support {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::collections::HashMap;
    use std::sync::Mutex;

    use crate::knowledge::{
        ChunkFilter, EntityRecord, Fact, KnowledgeError, KnowledgeStore, Predicate, ScoredHit,
        VectorSpace,
    };

    /// Deterministic embedder fake (§14): whitespace token counting +2 for
    /// specials, vectors derived from a word hash — chunks sharing vocabulary
    /// score similar, so fusion-order assertions are meaningful. Counts embed
    /// calls.
    pub struct FakeEmbedder {
        calls: Mutex<usize>,
    }

    impl FakeEmbedder {
        pub fn new() -> Self {
            Self {
                calls: Mutex::new(0),
            }
        }

        pub fn embed_calls(&self) -> usize {
            *self.calls.lock().unwrap()
        }
    }

    impl Default for FakeEmbedder {
        fn default() -> Self {
            Self::new()
        }
    }

    impl crate::pipeline::Embedder for FakeEmbedder {
        fn model_id(&self) -> &'static str {
            "fake-embedder"
        }

        fn dim(&self) -> usize {
            4
        }

        fn max_input_tokens(&self) -> usize {
            512
        }

        fn count_tokens(&self, text: &str) -> usize {
            text.split_whitespace().count() + 2
        }

        fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, crate::pipeline::EmbedError> {
            *self.calls.lock().unwrap() += 1;
            Ok(texts
                .iter()
                .map(|t| {
                    // Word-hash bag: each word's initial lands in one of four
                    // buckets; normalized so cosine = lexical overlap.
                    let mut v = [0.0f32; 4];
                    for word in t.split_whitespace() {
                        let b = usize::from(word.as_bytes().first().copied().unwrap_or(b' '));
                        v[b % 4] += 1.0;
                    }
                    let norm = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2] + v[3] * v[3]).sqrt();
                    if norm > 0.0 {
                        v.map(|x| x / norm).to_vec()
                    } else {
                        v.to_vec()
                    }
                })
                .collect())
        }
    }

    /// One stored vector: `(doc_id, vector)`.
    type StoredVector = (String, Vec<f32>);

    /// The §8 Stage 4 aggregation a [`KnowledgeStore`] fake must hold for
    /// fact-merge assertions.
    type StoredFact = (
        String,      // subject_id
        Predicate,   // predicate
        String,      // object_id
        u64,         // support_count
        Vec<String>, // evidence chunk ids (capped)
        Vec<String>, // occurrences (capped)
    );

    /// The in-memory `KnowledgeStore` fake (§14): vectors keyed by
    /// `(space, id)` with document membership, brute-force cosine KNN, plus the
    /// graph state Stage 4 writes (entities, `:MENTIONS`, fact edges with the
    /// §8 aggregation). Graph reads mirror the postconditions of the real impl.
    pub struct InMemoryKnowledge {
        vectors: Mutex<HashMap<(String, String), StoredVector>>,
        entities: Mutex<HashMap<String, EntityRecord>>,
        mentions: Mutex<std::collections::HashSet<(String, String)>>,
        facts: Mutex<HashMap<(String, String, String), StoredFact>>,
        graph_traversal: bool,
    }

    impl Default for InMemoryKnowledge {
        fn default() -> Self {
            Self {
                vectors: Mutex::new(HashMap::new()),
                entities: Mutex::new(HashMap::new()),
                mentions: Mutex::new(std::collections::HashSet::new()),
                facts: Mutex::new(HashMap::new()),
                // The fake mirrors the real store's reads (§14), so it declares
                // the same capability by default.
                graph_traversal: true,
            }
        }
    }

    fn key(space: &VectorSpace, id: &str) -> (String, String) {
        (format!("{space:?}"), id.to_string())
    }

    impl InMemoryKnowledge {
        /// The capability-off variant: a store without graph traversal, for
        /// the §8 Stage 5 degradation tests.
        #[must_use]
        pub fn without_graph() -> Self {
            Self {
                graph_traversal: false,
                ..Self::default()
            }
        }

        /// Test helper: simulates a lost vector (§7.3 repair path).
        pub fn remove(&self, id: &str) {
            self.vectors.lock().unwrap().retain(|(_, key), _| key != id);
        }

        /// Test helper: all fact edges currently stored.
        pub fn facts(&self) -> Vec<StoredFact> {
            self.facts.lock().unwrap().values().cloned().collect()
        }

        fn cosine(a: &[f32], b: &[f32]) -> f32 {
            let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
            let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
            let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
            if na == 0.0 || nb == 0.0 {
                0.0
            } else {
                dot / (na * nb)
            }
        }
    }

    #[async_trait::async_trait]
    impl KnowledgeStore for InMemoryKnowledge {
        fn capabilities(&self) -> crate::knowledge::KsCapabilities {
            crate::knowledge::KsCapabilities {
                filtered_ann: false,
                graph_traversal: self.graph_traversal,
            }
        }

        async fn upsert_vectors(
            &self,
            space: VectorSpace,
            doc_id: &str,
            ids: &[&str],
            vectors: &[Vec<f32>],
        ) -> Result<(), KnowledgeError> {
            let mut store = self.vectors.lock().unwrap();
            for (id, v) in ids.iter().zip(vectors) {
                store.insert(key(&space, id), (doc_id.to_string(), v.clone()));
            }
            Ok(())
        }

        async fn knn(
            &self,
            space: VectorSpace,
            q: &[f32],
            k: usize,
            _f: &ChunkFilter,
        ) -> Result<Vec<ScoredHit>, KnowledgeError> {
            let store = self.vectors.lock().unwrap();
            let mut hits: Vec<ScoredHit> = store
                .iter()
                .filter(|((s, _), _)| *s == format!("{space:?}"))
                .map(|((_, id), (_, v))| ScoredHit {
                    id: id.clone(),
                    score: Self::cosine(q, v),
                })
                .collect();
            hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.id.cmp(&b.id)));
            hits.truncate(k);
            Ok(hits)
        }

        async fn has_vector(&self, space: VectorSpace, id: &str) -> Result<bool, KnowledgeError> {
            Ok(self.vectors.lock().unwrap().contains_key(&key(&space, id)))
        }

        async fn upsert_entity(&self, e: &EntityRecord) -> Result<(), KnowledgeError> {
            self.entities
                .lock()
                .unwrap()
                .insert(e.entity_id.clone(), e.clone());
            Ok(())
        }

        async fn link_mention(
            &self,
            chunk_id: &str,
            entity_id: &str,
        ) -> Result<(), KnowledgeError> {
            self.mentions
                .lock()
                .unwrap()
                .insert((chunk_id.to_string(), entity_id.to_string()));
            Ok(())
        }

        async fn fold_entity(&self, loser: &str, winner: &str) -> Result<(), KnowledgeError> {
            if loser == winner {
                return Ok(());
            }

            self.entities.lock().unwrap().remove(loser);

            let mut mentions = self.mentions.lock().unwrap();
            let rewired: Vec<(String, String)> = mentions
                .iter()
                .filter(|(_, entity)| entity == loser)
                .map(|(chunk, _)| (chunk.clone(), winner.to_string()))
                .collect();
            mentions.retain(|(_, entity)| entity != loser);
            mentions.extend(rewired);
            drop(mentions);

            let mut facts = self.facts.lock().unwrap();
            let existing = std::mem::take(&mut *facts);
            for (_, (subject, predicate, object, support, evidence, occurrences)) in existing {
                let subject = if subject == loser {
                    winner.to_string()
                } else {
                    subject
                };
                let object = if object == loser {
                    winner.to_string()
                } else {
                    object
                };
                let key = (
                    subject.clone(),
                    predicate.as_str().to_string(),
                    object.clone(),
                );
                let entry = facts.entry(key).or_insert_with(|| {
                    (
                        subject.clone(),
                        predicate,
                        object.clone(),
                        0,
                        Vec::new(),
                        Vec::new(),
                    )
                });
                entry.3 = entry.3.saturating_add(support);
                for item in evidence {
                    if !entry.4.iter().any(|existing| existing == &item) {
                        entry.4.push(item);
                    }
                }
                for item in occurrences {
                    if !entry.5.iter().any(|existing| existing == &item) {
                        entry.5.push(item);
                    }
                }
            }
            Ok(())
        }

        async fn merge_fact(
            &self,
            subject_id: &str,
            predicate: Predicate,
            object_id: &str,
            evidence_chunk: &str,
            properties: Option<&serde_json::Value>,
            caps: crate::knowledge::FactCaps,
        ) -> Result<(), KnowledgeError> {
            let identity = (
                subject_id.to_string(),
                predicate.as_str().to_string(),
                object_id.to_string(),
            );
            let mut facts = self.facts.lock().unwrap();
            let entry = facts.entry(identity).or_insert_with(|| {
                (
                    subject_id.to_string(),
                    predicate,
                    object_id.to_string(),
                    0,
                    Vec::new(),
                    Vec::new(),
                )
            });
            // §8 aggregation mirror: support increments on new evidence; lists
            // capped + deduped; replay with the same chunk is a no-op.
            if !entry.4.iter().any(|c| c == evidence_chunk) {
                entry.3 += 1;
                if entry.4.len() < caps.max_evidence {
                    entry.4.push(evidence_chunk.to_string());
                }
            }
            if let Some(props) = properties {
                let values: Vec<String> = props.get("occurred_on").map_or_else(Vec::new, |v| {
                    v.as_array()
                        .map_or_else(Vec::new, |arr| {
                            arr.iter()
                                .filter_map(serde_json::Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .into_iter()
                        .chain(v.as_str().map(str::to_string))
                        .collect()
                });
                for value in values {
                    if !entry.5.iter().any(|o| o == &value) && entry.5.len() < caps.max_occurrences
                    {
                        entry.5.push(value);
                    }
                }
            }
            Ok(())
        }

        async fn delete_doc(&self, doc_id: &str) -> Result<(), KnowledgeError> {
            let mut deleted_chunks = std::collections::HashSet::new();
            self.vectors.lock().unwrap().retain(|(space, id), (d, _)| {
                let keep = d != doc_id;
                if !keep && space.starts_with("Chunks") {
                    deleted_chunks.insert(id.clone());
                }
                keep
            });
            self.mentions
                .lock()
                .unwrap()
                .retain(|(chunk, _)| !deleted_chunks.contains(chunk));
            Ok(())
        }

        async fn chunks_for_entities(&self, ids: &[&str]) -> Result<Vec<String>, KnowledgeError> {
            let mentions = self.mentions.lock().unwrap();
            let mut chunks: Vec<String> = mentions
                .iter()
                .filter(|(_, entity)| ids.iter().any(|id| id == entity))
                .map(|(chunk, _)| chunk.clone())
                .collect();
            chunks.sort();
            chunks.dedup();
            Ok(chunks)
        }

        async fn facts_within_hops(
            &self,
            ids: &[&str],
            hops: u8,
        ) -> Result<Vec<Fact>, KnowledgeError> {
            if ids.is_empty() || hops == 0 {
                return Ok(Vec::new());
            }
            let facts = self.facts.lock().unwrap();
            let mut frontier: std::collections::HashSet<String> =
                ids.iter().map(|s| (*s).to_string()).collect();
            let mut out: Vec<Fact> = Vec::new();
            let mut seen: std::collections::HashSet<(String, String, String)> =
                std::collections::HashSet::new();
            for _ in 0..hops {
                let mut next = std::collections::HashSet::new();
                for (subj, pred, obj, support, _evidence, _occ) in facts.values() {
                    if frontier.contains(subj) || frontier.contains(obj) {
                        if seen.insert((subj.clone(), pred.as_str().to_string(), obj.clone())) {
                            out.push(Fact {
                                subject_id: subj.clone(),
                                predicate: *pred,
                                object_id: obj.clone(),
                                support_count: *support,
                                properties: None,
                            });
                        }
                        next.insert(subj.clone());
                        next.insert(obj.clone());
                    }
                }
                if next.is_empty() {
                    break;
                }
                frontier = next;
            }
            Ok(out)
        }
    }
}
