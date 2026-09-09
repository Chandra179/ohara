//! Operator query orchestration. Keeping this startup path separate from the
//! worker loop makes its provider lifecycle and error taxonomy local.

use crate::config::Config;
use crate::control;
use crate::knowledge::ModelId;

use super::runtime::query_ports;
use super::{IdentityReranker, Retriever, ScoredChunk, WhatlangNormalizer};

/// Operator query failures. Provider construction failures are reported as boot
/// errors so the CLI and the worker share the same startup diagnostics.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    /// The query must contain at least one non-whitespace character.
    #[error("query must not be empty")]
    Empty,
    /// A zero result limit cannot produce a useful operator response.
    #[error("query top_k must be greater than zero")]
    InvalidTopK,
    /// The requested result count exceeds the configured candidate pool.
    #[error("query top_k {requested} exceeds retrieval.pool {pool}")]
    TopKExceedsPool {
        /// Requested result count.
        requested: usize,
        /// Configured retrieval candidate pool.
        pool: usize,
    },
    /// The configured data or database parent directory could not be created.
    #[error("query startup I/O: {0}")]
    Io(#[from] std::io::Error),
    /// A configured provider or store could not be opened.
    #[error("query startup: {0}")]
    Boot(#[from] crate::BootError),
    /// The retrieval paths failed after startup.
    #[error("query retrieval: {0}")]
    Retrieve(#[from] super::RetrieveError),
}

/// Runs the operator retrieval path with the configured local providers.
///
/// Querying does not boot the worker, claim jobs, or health-check the extraction
/// LLM. It does use the same validated retrieval configuration and stores.
///
/// # Errors
/// [`QueryError`] for invalid input, provider startup, or retrieval failures.
pub async fn query(
    config: Config,
    query_text: &str,
    top_k: usize,
) -> Result<Vec<ScoredChunk>, QueryError> {
    if query_text.trim().is_empty() {
        return Err(QueryError::Empty);
    }
    if top_k == 0 {
        return Err(QueryError::InvalidTopK);
    }

    let config = std::sync::Arc::new(config);
    if top_k > config.retrieval().pool() {
        return Err(QueryError::TopKExceedsPool {
            requested: top_k,
            pool: config.retrieval().pool(),
        });
    }
    std::fs::create_dir_all(config.data_dir())?;
    if let Some(parent) = config.db_path().parent() {
        std::fs::create_dir_all(parent)?;
    }

    let ports = query_ports(&config)?;
    let conn = control::connect(config.db_path()).map_err(crate::BootError::from)?;
    let normalizer = WhatlangNormalizer::new(config.retrieval().detection_confidence_floor());
    let reranker = IdentityReranker;
    let retriever = Retriever::new(
        &conn,
        ports.knowledge.as_ref(),
        ports.embedder.as_ref(),
        &normalizer,
        &reranker,
        ModelId::new(config.knowledge().read_model()),
        config.retrieval().clone(),
    );

    retriever
        .query(query_text, top_k)
        .await
        .map_err(QueryError::Retrieve)
}
