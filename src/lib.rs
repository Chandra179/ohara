//! ohara — an embedded, zero-daemon pipeline: web scrape → clean → chunk/vectorize
//! → graph extraction → `GraphRAG` retrieval, all in one Rust process.
//!
//! `SQLite` (WAL) is the control plane ([`control`]), `LadybugDB` is the knowledge
//! plane ([`knowledge`]), and the fetch ladder ([`engine`]) is the only moving
//! part (§1.1). The full system design lives in
//! [docs/ARCHITECTURE.md](https://github.com/Chandra179/ohara/blob/main/docs/ARCHITECTURE.md);
//! code style, API design, and lint policy live in `docs/CODE_GUIDE.md`.
//!
//! # Examples
//!
//! Boot with defaults and run the worker loop until interrupted:
//!
//! ```no_run
//! use ohara::config::Config;
//!
//! # async fn example() -> Result<(), ohara::BootError> {
//! let config = Config::load(None)?; // defaults; data/ under the working directory
//! ohara::run(config).await?;        // claims jobs, dispatches stages, until Ctrl-C
//! # Ok(())
//! # }
//! ```
//!
//! Drive the control plane directly — enqueue a URL, claim its SCRAPE job, and
//! complete it with the §6 stage chain:
//!
//! ```
//! use ohara::control::{self, Completion, NewDocument, Stage};
//! use std::path::Path;
//!
//! # fn example() -> Result<(), ohara::control::DbError> {
//! let conn = control::connect(Path::new(":memory:"))?;
//! let outcome = control::insert_new(
//!     &conn,
//!     Path::new("data"),
//!     &NewDocument {
//!         source_url: "https://example.com/post".to_string(),
//!         source_url_normalized: "https://example.com/post".to_string(),
//!         priority: 5,
//!         pipeline_version: env!("CARGO_PKG_VERSION").to_string(),
//!     },
//!     "2026-09-06 12:00:00",
//! )?;
//!
//! let Some(job) = control::claim_next(
//!     &conn,
//!     Stage::Scrape,
//!     "worker-1",
//!     "2026-09-06 12:00:01",
//!     60,
//! )? else {
//!     return Ok(());
//! };
//! assert_eq!(job.doc_id(), outcome.doc_id());
//!
//! // DONE + milestone (SCRAPED) + the chained CLEAN job: one transaction (§6).
//! control::complete(&conn, Stage::Scrape, &job, Completion::Chain, "2026-09-06 12:00:02")?;
//! # Ok(())
//! # }
//! # fn main() -> Result<(), ohara::control::DbError> {
//! #     example()
//! # }
//! ```

pub mod config;
pub mod control;
pub mod engine;
pub mod knowledge;
pub mod llm;
pub mod ops;
pub mod pipeline;
pub mod text;

pub use pipeline::run;

/// Errors that abort startup before the worker loop begins (§10: fail fast at
/// boot, never mid-stage).
#[derive(Debug, thiserror::Error)]
pub enum BootError {
    /// A knob failed validation.
    #[error("invalid configuration: {0}")]
    Config(#[from] config::ConfigError),
    /// The control store could not be opened or migrated.
    #[error("control store: {0}")]
    Control(#[from] control::DbError),
    /// A data directory could not be created.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// The fetcher could not be built at boot.
    #[error("fetcher: {0}")]
    Fetch(#[from] engine::FetchError),
    /// The embedder could not be built or loaded at boot (§4).
    #[error("embedder: {0}")]
    Embed(#[from] pipeline::EmbedError),
    /// The knowledge store could not be opened at boot (§3).
    #[error("knowledge store: {0}")]
    Knowledge(#[from] knowledge::KnowledgeError),
    /// The LLM endpoint could not be built or health-checked at boot (§2).
    #[error("llm: {0}")]
    Llm(#[from] llm::LlmError),
    /// The worker loop task failed unexpectedly.
    #[error("worker loop failed: {0}")]
    Worker(String),
    /// A `Fatal` stage error drained the worker (§6); the boot sweep finishes
    /// recovery on the next start.
    #[error("fatal stage failure: {0}")]
    Fatal(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Retry classification shared by every error taxonomy (§10): port errors expose
/// `class()`, and the stage layer maps classes onto the §6 job state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Class {
    /// Transient — retry with backoff (§6).
    Retry,
    /// Permanent — retrying cannot succeed; the job goes `DEAD` (§6).
    Permanent,
    /// Fatal — stop scheduling, drain, reconcile at next boot (§6).
    Fatal,
}
