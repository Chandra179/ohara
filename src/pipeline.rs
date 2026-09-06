//! Worker loop (§6): claim jobs, dispatch stages, classify failures, record every
//! outcome in `stage_events`. Single worker by default — honest for an embedded
//! tool (§6); the lease protocol makes multi-worker safe when needed.

mod chunk;
mod clean;
mod embed;
mod extract;
mod retrieve;
mod scrape;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::Class;
use crate::config::Config;
use crate::control::{self, ClaimedJob, Completion, DbError, Stage};
use crate::engine::Fetcher;
use crate::knowledge::KnowledgeStore;

pub use crate::engine::HttpFetcher;
#[cfg(feature = "ladybug")]
pub use crate::knowledge::LadybugStore;
pub use chunk::{Chunk, chunk_document};
pub use clean::{CleanOutcome, ExtractError, ExtractedArticle, Extractor, ReadabilityExtractor};
#[cfg(feature = "onnx-embedder")]
pub use embed::LocalEmbedder;
pub use embed::{EmbedError, Embedder};
pub use retrieve::{Lang, QueryNormalizer, RerankError, Reranker, ScoredChunk};

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

/// What a stage needs for one claimed job (§1.3): validated config, the store
/// connection (held by the tick), the async runtime handle for port calls, and
/// the ports themselves — fakes substitute cleanly in tests (§14).
pub(crate) struct StageCtx<'a> {
    pub(crate) config: &'a Config,
    pub(crate) conn: &'a rusqlite::Connection,
    /// Handle for `block_on`-ing async port calls from the blocking thread.
    pub(crate) handle: &'a tokio::runtime::Handle,
    pub(crate) fetcher: &'a dyn Fetcher,
    pub(crate) extractor: &'a dyn Extractor,
    pub(crate) embedder: &'a dyn Embedder,
    pub(crate) knowledge: &'a dyn KnowledgeStore,
}

/// What the loop does after one claimed job's outcome is recorded.
enum Flow {
    /// Keep scheduling.
    Continue,
    /// A `Fatal` classification (§6): stop scheduling, drain, reconcile at next boot.
    Abort(Box<StageError>),
}

/// The worker: owns the control connection, the validated configuration, and the
/// ports. The connection sits behind a [`Mutex`] — `rusqlite::Connection`
/// is `Send` but not `Sync`, and the loop reaches it from blocking tasks.
pub struct Worker {
    config: Arc<Config>,
    conn: Mutex<rusqlite::Connection>,
    handle: tokio::runtime::Handle,
    fetcher: Arc<dyn Fetcher>,
    extractor: Arc<dyn Extractor>,
    embedder: Arc<dyn Embedder>,
    knowledge: Arc<dyn KnowledgeStore>,
    id: String,
}

/// The default [`Embedder`]: the pinned local ONNX model (§4). Requires the
/// `onnx-embedder` feature — without it, inject a provider via
/// [`Worker::with_ports`] (§9 provider swap).
#[cfg(feature = "onnx-embedder")]
fn default_embedder(config: &Config) -> Result<Arc<dyn Embedder>, crate::BootError> {
    Ok(Arc::new(LocalEmbedder::new(
        &config.data_dir().join("models"),
    )?))
}

/// The default [`KnowledgeStore`]: the embedded `LadybugDB` engine (§3). Requires
/// the `ladybug` feature — without it, inject an alternative backend via
/// [`Worker::with_ports`] (§9 swap candidates).
#[cfg(feature = "ladybug")]
fn default_knowledge(config: &Config) -> Result<Arc<dyn KnowledgeStore>, crate::BootError> {
    std::fs::create_dir_all(config.data_dir()).map_err(|e| {
        crate::BootError::Worker(format!(
            "cannot create data dir {}: {e}",
            config.data_dir().display()
        ))
    })?;
    Ok(Arc::new(LadybugStore::open(
        &config.data_dir().join("ladybug"),
        config.embedder().dim(),
    )?))
}

#[cfg(not(feature = "onnx-embedder"))]
fn default_embedder(_config: &Config) -> Result<Arc<dyn Embedder>, crate::BootError> {
    Err(crate::BootError::Worker(
        "ohara was built without the `onnx-embedder` feature; provide an Embedder via Worker::with_ports (§9 provider swap)".to_string(),
    ))
}

#[cfg(not(feature = "ladybug"))]
fn default_knowledge(_config: &Config) -> Result<Arc<dyn KnowledgeStore>, crate::BootError> {
    Err(crate::BootError::Worker(
        "ohara was built without the `ladybug` feature; provide a KnowledgeStore via Worker::with_ports (§9 swap candidates)".to_string(),
    ))
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
        let fetcher = Arc::new(HttpFetcher::new(crate::engine::HttpFetcherParams {
            user_agent: config.fetcher().user_agent().to_string(),
            timeout: config.fetcher().timeout(),
            rate_limit: config.rate_limit(),
            allow_private_hosts: config.fetcher().allow_private_hosts(),
        })?);
        let extractor = Arc::new(ReadabilityExtractor);
        let embedder = default_embedder(&config)?;
        let knowledge = default_knowledge(&config)?;
        Self::with_ports(config, fetcher, extractor, embedder, knowledge)
    }

    /// Boots a worker with explicit ports (§14 integration: canned fetcher, fake
    /// extractor; §9: remote providers). Must be called inside a tokio runtime —
    /// stage bodies drive async port calls from blocking threads via its handle.
    ///
    /// # Errors
    /// [`crate::BootError`] if the store cannot be opened or migrated, or if no
    /// tokio runtime is active.
    pub fn with_ports(
        config: Arc<Config>,
        fetcher: Arc<dyn Fetcher>,
        extractor: Arc<dyn Extractor>,
        embedder: Arc<dyn Embedder>,
        knowledge: Arc<dyn KnowledgeStore>,
    ) -> Result<Self, crate::BootError> {
        let id = format!("worker-{}", std::process::id());
        let conn = control::connect(config.db_path())?;
        let handle = tokio::runtime::Handle::try_current().map_err(|_| {
            crate::BootError::Worker("ohara must run inside a tokio runtime".to_string())
        })?;
        Ok(Self {
            config,
            conn: Mutex::new(conn),
            handle,
            fetcher,
            extractor,
            embedder,
            knowledge,
            id,
        })
    }

    /// Locks the control connection. A poisoned lock (a panic while a store call
    /// was in flight) is recovered: the store itself is transactional, so the
    /// next statement runs against a consistent state (§1.2.4 idempotency).
    fn conn(&self) -> MutexGuard<'_, rusqlite::Connection> {
        self.conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Runs the §7.3 boot reconciliation sweep before the loop claims anything:
    /// interrupted §7.6 deletions re-execute, the §5 audit trail is pruned to
    /// retention. Expired leases are deliberately not swept — the §6 claim
    /// reclaims them with the correct accounting.
    ///
    /// # Errors
    /// [`DbError`] on store failure.
    pub fn reconcile(&self) -> Result<control::ReconcileReport, DbError> {
        control::reconcile(
            &self.conn(),
            &control::now(),
            self.config.stage_events_retention(),
        )
    }

    /// One scheduling step: claim the next runnable job in pipeline order,
    /// execute it, record the outcome. One job per call — the run loop spins
    /// back immediately while the queue drains, and tests/hosts can advance the
    /// pipeline step by step ([`Worker::tick_once`]).
    ///
    /// # Errors
    /// [`DbError`] on store failure; a `Fatal` stage error aborts scheduling (§6)
    /// and is returned as the tick's [`Flow`].
    fn tick(&self) -> Result<(usize, Flow), DbError> {
        let conn = self.conn();
        let now = control::now();
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
            let flow = self.execute(&conn, stage, &job)?;
            return Ok((1, flow));
        }
        Ok((0, Flow::Continue))
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
            (_, Flow::Abort(err)) => Err(crate::BootError::Fatal(err)),
            (executed, Flow::Continue) => Ok(executed),
        }
    }

    /// Executes one claimed job and records the outcome (§6, §10). Panics are
    /// caught: the `PANIC` audit row is written and the job is left `RUNNING` —
    /// its lease expiry makes it reclaimable, punishing the run exactly once (§6).
    ///
    /// # Errors
    /// [`DbError`] on store failure (transitions and audit writes).
    fn execute(
        &self,
        conn: &rusqlite::Connection,
        stage: Stage,
        job: &ClaimedJob,
    ) -> Result<Flow, DbError> {
        let now = control::now();
        let ctx = StageCtx {
            config: &self.config,
            conn,
            handle: &self.handle,
            fetcher: self.fetcher.as_ref(),
            extractor: self.extractor.as_ref(),
            embedder: self.embedder.as_ref(),
            knowledge: self.knowledge.as_ref(),
        };
        let outcome = catch_unwind(AssertUnwindSafe(|| dispatch(stage, &ctx, job)));
        match outcome {
            Ok(Ok(outcome)) => {
                // §6 stage chaining: milestone + successor job in one transaction,
                // decided here (the worker owns config and the stage's outcome).
                let completion = match (outcome, stage) {
                    (StageOutcome::Stop, _) => Completion::Done,
                    (StageOutcome::Advance, Stage::Vectorize)
                        if !self.config.pipeline().graph_enabled() =>
                    {
                        Completion::Milestone
                    }
                    (StageOutcome::Advance, _) => Completion::Chain,
                };
                control::complete(conn, stage, job, completion, &now)?;
                control::record_event(
                    conn,
                    Some(job.doc_id()),
                    Some(job.job_id()),
                    Some(stage.as_str()),
                    "DONE",
                    None,
                )?;
                Ok(Flow::Continue)
            }
            Ok(Err(err)) => match &err {
                StageError::Transient { .. } => {
                    let attempts_after = job.attempts() + 1;
                    if attempts_after >= job.max_attempts() {
                        Self::dead(conn, job, stage, &err.to_string(), &now)?;
                    } else {
                        let due = control::now_plus(&now, self.backoff_secs(attempts_after))?;
                        control::retry(conn, job.job_id(), &due, &err.to_string(), &now)?;
                        control::record_event(
                            conn,
                            Some(job.doc_id()),
                            Some(job.job_id()),
                            Some(stage.as_str()),
                            "RETRY",
                            Some(&err.to_string()),
                        )?;
                    }
                    Ok(Flow::Continue)
                }
                StageError::Permanent { .. } => {
                    Self::dead(conn, job, stage, &err.to_string(), &now)?;
                    Ok(Flow::Continue)
                }
                StageError::Fatal { .. } => {
                    control::record_event(
                        conn,
                        Some(job.doc_id()),
                        Some(job.job_id()),
                        Some(stage.as_str()),
                        "FATAL",
                        Some(&err.to_string()),
                    )?;
                    Ok(Flow::Abort(Box::new(err)))
                }
            },
            Err(panic) => {
                // §10 worker isolation: record and continue; no transition — the
                // expired lease reclaims the job with `last_error = 'lease expired'`.
                let detail = panic_detail(&panic);
                control::record_event(
                    conn,
                    Some(job.doc_id()),
                    Some(job.job_id()),
                    Some(stage.as_str()),
                    "PANIC",
                    Some(&detail),
                )?;
                Ok(Flow::Continue)
            }
        }
    }

    /// Records the §6 terminal mapping: job `DEAD`, document `FAILED`.
    fn dead(
        conn: &rusqlite::Connection,
        job: &ClaimedJob,
        stage: Stage,
        error: &str,
        now: &str,
    ) -> Result<(), DbError> {
        control::dead(conn, job.job_id(), job.doc_id(), error, now)?;
        control::record_event(
            conn,
            Some(job.doc_id()),
            Some(job.job_id()),
            Some(stage.as_str()),
            "DEAD",
            Some(error),
        )
    }

    /// Jittered exponential backoff (§6): `base · 2^(attempts−1)`, jittered up to
    /// +25% from the clock's sub-second nanos.
    fn backoff_secs(&self, attempts_after: i64) -> u64 {
        let base = self.config.backoff_base().as_secs();
        let exp = u32::try_from(attempts_after.saturating_sub(1))
            .unwrap_or(16)
            .min(16);
        let secs = base.saturating_mul(1 << exp);
        let jitter_salt = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |t| u64::from(t.subsec_nanos()));
        secs.saturating_add(jitter_salt % (secs / 4 + 1))
    }
}

/// Dispatches one claimed job to its stage. Stage bodies take their dependencies
/// through [`StageCtx`] (§1.3) — the ports are trait objects, so fakes substitute
/// cleanly in tests (§14).
fn dispatch(
    stage: Stage,
    ctx: &StageCtx<'_>,
    job: &ClaimedJob,
) -> Result<StageOutcome, StageError> {
    match stage {
        Stage::Scrape => scrape::run(ctx, job),
        Stage::Clean => clean::run(ctx, job),
        Stage::Vectorize => embed::run(ctx, job),
        Stage::Extract => extract::run(ctx.config, job),
    }
}

/// Best-effort panic payload extraction (§10: panics bypass normal audit — record
/// what we can).
fn panic_detail(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
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
    worker.reconcile()?;

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
        ChunkFilter, EntityRecord, Fact, KnowledgeError, KnowledgeStore, ScoredHit, VectorSpace,
    };

    /// An extractor no stage under test may call.
    pub struct NeverExtractor;

    impl crate::pipeline::Extractor for NeverExtractor {
        fn extract(
            &self,
            _html: &str,
            _base: &str,
        ) -> Result<crate::pipeline::ExtractedArticle, crate::pipeline::ExtractError> {
            panic!("stage must not extract")
        }
    }

    /// A fetcher no stage under test may call.
    pub struct NeverFetcher;

    #[async_trait::async_trait]
    impl crate::engine::Fetcher for NeverFetcher {
        fn capabilities(&self) -> crate::engine::FetchCapabilities {
            panic!("stage must not fetch")
        }

        async fn fetch_with_policy(
            &self,
            _url: &crate::engine::NormalizedUrl,
            _policy: &crate::engine::FetchPolicy,
        ) -> Result<crate::engine::FetchedDoc, crate::engine::FetchError> {
            panic!("stage must not fetch")
        }
    }

    /// An embedder no stage under test may call.
    pub struct NeverEmbedder;

    impl crate::pipeline::Embedder for NeverEmbedder {
        fn model_id(&self) -> &str {
            panic!("stage must not embed")
        }

        fn dim(&self) -> usize {
            panic!("stage must not embed")
        }

        fn count_tokens(&self, _text: &str) -> usize {
            panic!("stage must not count tokens")
        }

        fn embed(&self, _texts: &[&str]) -> Result<Vec<Vec<f32>>, crate::pipeline::EmbedError> {
            panic!("stage must not embed")
        }
    }

    /// A knowledge store no stage under test may call.
    pub struct NeverKnowledge;

    #[async_trait::async_trait]
    impl KnowledgeStore for NeverKnowledge {
        fn capabilities(&self) -> crate::knowledge::KsCapabilities {
            panic!("stage must not touch the knowledge plane")
        }

        async fn upsert_vectors(
            &self,
            _space: VectorSpace,
            _doc_id: &str,
            _ids: &[&str],
            _vectors: &[Vec<f32>],
        ) -> Result<(), KnowledgeError> {
            panic!("stage must not touch the knowledge plane")
        }

        async fn knn(
            &self,
            _space: VectorSpace,
            _q: &[f32],
            _k: usize,
            _f: &ChunkFilter,
        ) -> Result<Vec<ScoredHit>, KnowledgeError> {
            panic!("stage must not touch the knowledge plane")
        }

        async fn has_vector(&self, _space: VectorSpace, _id: &str) -> Result<bool, KnowledgeError> {
            panic!("stage must not touch the knowledge plane")
        }

        async fn upsert_entity(&self, _e: &EntityRecord) -> Result<(), KnowledgeError> {
            panic!("stage must not touch the knowledge plane")
        }

        async fn link_mention(
            &self,
            _chunk_id: &str,
            _entity_id: &str,
        ) -> Result<(), KnowledgeError> {
            panic!("stage must not touch the knowledge plane")
        }

        async fn fold_entity(&self, _loser: &str, _winner: &str) -> Result<(), KnowledgeError> {
            panic!("stage must not touch the knowledge plane")
        }

        async fn delete_doc(&self, _doc_id: &str) -> Result<(), KnowledgeError> {
            panic!("stage must not touch the knowledge plane")
        }

        async fn chunks_for_entities(&self, _ids: &[&str]) -> Result<Vec<String>, KnowledgeError> {
            panic!("stage must not touch the knowledge plane")
        }

        async fn facts_within_hops(
            &self,
            _ids: &[&str],
            _hops: u8,
        ) -> Result<Vec<Fact>, KnowledgeError> {
            panic!("stage must not touch the knowledge plane")
        }
    }

    /// Deterministic embedder fake (§14): whitespace token counting +2 for
    /// specials, vectors derived from the text's first byte. Counts embed calls.
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

        fn count_tokens(&self, text: &str) -> usize {
            text.split_whitespace().count() + 2
        }

        fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, crate::pipeline::EmbedError> {
            *self.calls.lock().unwrap() += 1;
            Ok(texts
                .iter()
                .map(|t| {
                    let seed = f32::from(t.as_bytes().first().copied().unwrap_or(b' '));
                    vec![seed % 8.0 + 1.0, seed % 5.0 + 1.0, 1.0, 0.5]
                })
                .collect())
        }
    }

    /// One stored vector: `(doc_id, vector)`.
    type StoredVector = (String, Vec<f32>);

    /// The in-memory `KnowledgeStore` fake (§14): vectors keyed by
    /// `(space, id)` with document membership, brute-force cosine KNN. Graph
    /// methods are out of scope until Stage 4 tests (§15 step 6).
    #[derive(Default)]
    pub struct InMemoryKnowledge {
        vectors: Mutex<HashMap<(String, String), StoredVector>>,
    }

    fn key(space: &VectorSpace, id: &str) -> (String, String) {
        (format!("{space:?}"), id.to_string())
    }

    impl InMemoryKnowledge {
        /// Test helper: simulates a lost vector (§7.3 repair path).
        pub fn remove(&self, id: &str) {
            self.vectors.lock().unwrap().retain(|(_, key), _| key != id);
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
                graph_traversal: false,
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

        async fn upsert_entity(&self, _e: &EntityRecord) -> Result<(), KnowledgeError> {
            Ok(())
        }

        async fn link_mention(
            &self,
            _chunk_id: &str,
            _entity_id: &str,
        ) -> Result<(), KnowledgeError> {
            Ok(())
        }

        async fn fold_entity(&self, _loser: &str, _winner: &str) -> Result<(), KnowledgeError> {
            Ok(())
        }

        async fn delete_doc(&self, doc_id: &str) -> Result<(), KnowledgeError> {
            self.vectors.lock().unwrap().retain(|_, (d, _)| d != doc_id);
            Ok(())
        }

        async fn chunks_for_entities(&self, _ids: &[&str]) -> Result<Vec<String>, KnowledgeError> {
            Ok(Vec::new())
        }

        async fn facts_within_hops(
            &self,
            _ids: &[&str],
            _hops: u8,
        ) -> Result<Vec<Fact>, KnowledgeError> {
            Ok(Vec::new())
        }
    }
}
