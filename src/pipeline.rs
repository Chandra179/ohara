//! Worker loop (§6): claim jobs, dispatch stages, classify failures, record every
//! outcome in `stage_events`. Single worker by default — honest for an embedded
//! tool (§6); the lease protocol makes multi-worker safe when needed.

mod chunk;
mod clean;
mod embed;
mod execution;
mod extract;
mod extract_graph;
mod extraction_contract;
mod query;
mod recovery;
mod retrieve;
mod scrape;
mod synthesis;
mod usage;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::Class;
use crate::config::Config;
use crate::control::{self, ControlDb, DbError, Stage};
use crate::engine::Fetcher;
use crate::knowledge::KnowledgeStore;
use crate::llm::Llm;
use crate::runtime;

pub use chunk::{Chunk, chunk_document};
pub use clean::{CleanOutcome, ExtractError, ExtractedArticle, Extractor, ReadabilityExtractor};
#[cfg(feature = "onnx-embedder")]
pub use embed::LocalEmbedder;
pub use embed::{EmbedError, Embedder};
pub use query::{QueryError, QueryResponse, answer, query};
pub(crate) use query::{QueryPorts, answer_with_ports, validate_query};
#[cfg(feature = "onnx-embedder")]
pub use retrieve::LocalReranker;
pub use retrieve::{
    IdentityReranker, Lang, QueryEntity, QueryEntitySource, QueryNormalizer, RerankError, Reranker,
    RetrieveError, RetrievedContext, Retriever, ScoredChunk, WhatlangNormalizer,
    fts_match_expression,
};
pub use synthesis::{QueryAvailability, QueryGrounding};

/// Checks the default embedder's local readiness without downloading model
/// files. The server uses this through the pipeline facade so it does not know
/// the embedder implementation's cache layout.
#[cfg(feature = "onnx-embedder")]
pub(crate) fn check_embedder_readiness(config: &Config) -> Result<(), EmbedError> {
    if config.embedder().model_id() != "bge-small-en-v1.5" {
        return Err(EmbedError::Unavailable(format!(
            "configured embedder model {:?} is unsupported by the built-in local provider",
            config.embedder().model_id()
        )));
    }
    embed::LocalEmbedder::check_cache(&config.data_dir().join("models"))
}

#[cfg(not(feature = "onnx-embedder"))]
pub(crate) fn check_embedder_readiness(_config: &Config) -> Result<(), EmbedError> {
    Err(EmbedError::Unavailable(
        "ohara was built without the `onnx-embedder` feature; use the default feature set or inject an Embedder".to_string(),
    ))
}

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
    heartbeat_conn: Mutex<ControlDb>,
    handle: tokio::runtime::Handle,
    ports: runtime::WorkerPorts,
    id: String,
    _runtime_lock: crate::ops::RuntimeLock,
}

impl Worker {
    /// Asynchronously boots a worker with the real ports: engine-plane fetch
    /// ladder (HTTP plus browser-profile impersonation), the readability
    /// extractor, the pinned local embedder, and the Qdrant/FalkorDB knowledge
    /// services. Callers create the data directories first ([`run`] does).
    ///
    /// # Errors
    /// [`crate::BootError`] if the store cannot be opened/migrated, the runtime
    /// handle is unavailable, or any default port cannot be built.
    pub async fn new(config: Arc<Config>) -> Result<Self, crate::BootError> {
        let runtime_lock = runtime::acquire_lock(&config)?;
        let ports = runtime::worker_ports(&config).await?;
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
        let id = format!("worker-{}", uuid::Uuid::now_v7());
        let conn = control::connect(config.db_path())?;
        let heartbeat_conn = control::connect(config.db_path())?;
        let handle = tokio::runtime::Handle::try_current().map_err(|_| {
            crate::BootError::Worker("ohara must run inside a tokio runtime".to_string())
        })?;
        control::register_worker(&conn, &id, std::process::id(), &control::now())?;
        Ok(Self {
            config,
            conn: Mutex::new(conn),
            heartbeat_conn: Mutex::new(heartbeat_conn),
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

    fn set_worker_state(
        &self,
        state: control::WorkerState,
        current_stage: Option<&str>,
        current_job_id: Option<&str>,
        last_error: Option<&str>,
    ) -> Result<(), crate::BootError> {
        let conn = self.conn();
        control::update_worker_state(
            &conn,
            &self.id,
            state,
            current_stage,
            current_job_id,
            last_error,
            &control::now(),
        )?;
        Ok(())
    }

    fn heartbeat(&self) -> Result<(), crate::BootError> {
        let conn = self
            .heartbeat_conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        control::heartbeat_worker(&conn, &self.id, &control::now())?;
        Ok(())
    }

    fn spawn_heartbeat(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let worker = Arc::clone(self);
        let interval = heartbeat_interval(worker.config.lease_ttl());
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                let worker = Arc::clone(&worker);
                match tokio::task::spawn_blocking(move || worker.heartbeat()).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => eprintln!("ohara: worker heartbeat failed: {error}"),
                    Err(error) => eprintln!("ohara: worker heartbeat task failed: {error}"),
                }
            }
        })
    }

    /// Runs the full §7.3 boot reconciliation sweep before the loop claims
    /// anything: interrupted §7.6 deletions are removed from the knowledge
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
            control::update_worker_state(
                &conn,
                &self.id,
                control::WorkerState::Running,
                Some(stage.as_str()),
                Some(job.job_id()),
                None,
                &now,
            )?;
            let executor =
                execution::StageExecutor::new(&self.config, &conn, &self.handle, &self.ports);
            let flow = executor.execute(stage, &job)?;
            control::update_worker_state(
                &conn,
                &self.id,
                control::WorkerState::Ready,
                None,
                None,
                None,
                &control::now(),
            )?;
            return Ok((1, flow));
        }
        control::update_worker_state(
            &conn,
            &self.id,
            control::WorkerState::Ready,
            None,
            None,
            None,
            &now,
        )?;
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
        let result = match self.tick().map_err(crate::BootError::Control)? {
            (_, execution::Flow::Abort(err)) => Err(crate::BootError::Fatal(err)),
            (executed, execution::Flow::Continue) => Ok(executed),
        };
        if let Err(error) = &result {
            let _ = self.set_worker_state(
                control::WorkerState::Failed,
                None,
                None,
                Some(&error.to_string()),
            );
        }
        result
    }
}

fn heartbeat_interval(lease_ttl: Duration) -> Duration {
    let interval = lease_ttl / 3;
    if interval.is_zero() {
        Duration::from_secs(1)
    } else {
        interval
    }
}

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
    eprintln!("ohara: worker booting providers");
    let worker = Arc::new(Worker::new(Arc::clone(&config)).await?);
    eprintln!("ohara: worker registered as {}", worker.id);
    let heartbeat = worker.spawn_heartbeat();
    let shutdown_requested = Arc::new(AtomicBool::new(false));
    let shutdown_listener = tokio::spawn(listen_for_shutdown(Arc::clone(&shutdown_requested)));
    let result: Result<(), crate::BootError> = async {
        worker.reconcile().await?;
        if shutdown_requested.load(Ordering::Acquire) {
            worker.set_worker_state(control::WorkerState::Stopping, None, None, None)?;
            return Ok(());
        }
        worker.set_worker_state(control::WorkerState::Ready, None, None, None)?;
        eprintln!("ohara: worker ready");

        loop {
            let executed = tokio::task::spawn_blocking({
                let worker = Arc::clone(&worker);
                move || worker.tick_once()
            })
            .await
            .map_err(|join| crate::BootError::Worker(join.to_string()))??;

            if shutdown_requested.load(Ordering::Acquire) {
                worker.set_worker_state(control::WorkerState::Stopping, None, None, None)?;
                break;
            }
            if executed == 0 {
                tokio::time::sleep(config.poll_interval()).await;
            }
            // Jobs executed: loop immediately to drain the queue.
        }
        Ok(())
    }
    .await;
    shutdown_listener.abort();
    heartbeat.abort();

    let (state, error) = match &result {
        Ok(()) => (control::WorkerState::Stopped, None),
        Err(error) => (control::WorkerState::Failed, Some(error.to_string())),
    };
    let _ = worker.set_worker_state(state, None, None, error.as_deref());
    result
}

async fn listen_for_shutdown(shutdown_requested: Arc<AtomicBool>) {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    result = tokio::signal::ctrl_c() => {
                        if let Err(error) = result {
                            eprintln!("ohara: failed to install Ctrl-C handler: {error}");
                        }
                    }
                    _ = terminate.recv() => {}
                }
            }
            Err(error) => {
                eprintln!("ohara: failed to install SIGTERM handler: {error}");
                if let Err(error) = tokio::signal::ctrl_c().await {
                    eprintln!("ohara: failed to install Ctrl-C handler: {error}");
                }
            }
        }
    }

    #[cfg(not(unix))]
    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("ohara: failed to install Ctrl-C handler: {error}");
    }

    shutdown_requested.store(true, Ordering::Release);
}

#[cfg(test)]
pub(crate) mod test_support {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::Mutex;

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
}
