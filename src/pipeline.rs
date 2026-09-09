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
use crate::control::{self, ClaimedJob, Completion, ControlDb, DbError, Stage};
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
#[cfg(feature = "onnx-embedder")]
pub use retrieve::LocalReranker;
pub use retrieve::{
    IdentityReranker, Lang, QueryEntity, QueryEntitySource, QueryNormalizer,
    RerankError, Reranker, RetrieveError, Retriever, ScoredChunk, WhatlangNormalizer,
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

/// What a stage needs for one claimed job (§1.3): validated config, the store
/// connection (held by the tick), the async runtime handle for port calls, and
/// the ports themselves — fakes substitute cleanly in tests (§14).
pub(crate) struct StageCtx<'a> {
    pub(crate) config: &'a Config,
    pub(crate) conn: &'a ControlDb,
    /// Handle for `block_on`-ing async port calls from the blocking thread.
    pub(crate) handle: &'a tokio::runtime::Handle,
    pub(crate) fetcher: &'a dyn Fetcher,
    pub(crate) extractor: &'a dyn Extractor,
    pub(crate) embedder: &'a dyn Embedder,
    pub(crate) knowledge: &'a dyn KnowledgeStore,
    pub(crate) llm: &'a dyn Llm,
}

/// What the loop does after one claimed job's outcome is recorded.
enum Flow {
    /// Keep scheduling.
    Continue,
    /// A `Fatal` classification (§6): stop scheduling, drain, reconcile at next boot.
    Abort(Box<StageError>),
}

/// The worker: owns the control database, the validated configuration, and the
/// ports. The database sits behind a [`Mutex`] because the embedded control
/// implementation serializes SQLite writes internally.
pub struct Worker {
    config: Arc<Config>,
    conn: Mutex<ControlDb>,
    handle: tokio::runtime::Handle,
    fetcher: Arc<dyn Fetcher>,
    extractor: Arc<dyn Extractor>,
    embedder: Arc<dyn Embedder>,
    knowledge: Arc<dyn KnowledgeStore>,
    llm: Arc<dyn Llm>,
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

/// The default [`Llm`]: the local Ollama provider (§2, §11.2), health-checked at
/// boot (§2 fail-fast) when the graph is enabled — without the graph, no stage
/// ever reaches the LLM, so deployments without a running Ollama stay bootable.
fn default_llm(config: &Config) -> Result<Arc<dyn Llm>, crate::BootError> {
    if !config.pipeline().graph_enabled() {
        return Ok(Arc::new(crate::llm::NoLlm));
    }
    let ollama = crate::llm::Ollama::new(
        config.llm().base_url().clone(),
        config.llm().health_timeout(),
    )?;
    let handle = tokio::runtime::Handle::try_current().map_err(|_| {
        crate::BootError::Worker(
            "ohara must run inside a tokio runtime to health-check the LLM endpoint".to_string(),
        )
    })?;
    handle.block_on(ollama.verify_endpoint())?;
    Ok(Arc::new(ollama))
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
            max_body_bytes: config.fetcher().max_body_bytes(),
            max_redirects: config.fetcher().max_redirects(),
        })?);
        let extractor = Arc::new(ReadabilityExtractor);
        let embedder = default_embedder(&config)?;
        let knowledge = default_knowledge(&config)?;
        let llm = default_llm(&config)?;
        Self::with_ports(config, fetcher, extractor, embedder, knowledge, llm)
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
        llm: Arc<dyn Llm>,
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
            llm,
            id,
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
        conn: &ControlDb,
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
            llm: self.llm.as_ref(),
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
        conn: &ControlDb,
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
        Stage::Extract => extract::run(ctx, job),
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
        ChunkFilter, EntityRecord, Fact, KnowledgeError, KnowledgeStore, Predicate, ScoredHit,
        VectorSpace,
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

    /// An LLM no stage under test may call.
    pub struct NeverLlm;

    #[async_trait::async_trait]
    impl crate::llm::Llm for NeverLlm {
        async fn complete(
            &self,
            _req: crate::llm::CompletionRequest,
        ) -> Result<crate::llm::CompletionResponse, crate::llm::LlmError> {
            panic!("stage must not call the LLM")
        }

        fn usage(&self) -> crate::llm::LlmUsage {
            panic!("stage must not call the LLM")
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

        async fn merge_fact(
            &self,
            _subject_id: &str,
            _predicate: crate::knowledge::Predicate,
            _object_id: &str,
            _evidence_chunk: &str,
            _properties: Option<&serde_json::Value>,
            _caps: crate::knowledge::FactCaps,
        ) -> Result<(), KnowledgeError> {
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

        async fn fold_entity(&self, _loser: &str, _winner: &str) -> Result<(), KnowledgeError> {
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
            self.vectors.lock().unwrap().retain(|_, (d, _)| d != doc_id);
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
