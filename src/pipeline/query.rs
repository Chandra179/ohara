//! Operator query orchestration. Keeping this startup path separate from the
//! worker loop makes its provider lifecycle and error taxonomy local.

use std::sync::Arc;

use crate::config::Config;
use crate::control::{self, ControlDb};
use crate::knowledge::{KnowledgeStore, ModelId};
use crate::llm::Llm;

use super::retrieve::RetrievedContext;
use super::synthesis;
use super::{Embedder, IdentityReranker, Retriever, ScoredChunk, WhatlangNormalizer};

/// The behavioral ports needed by operator retrieval and synthesis.
///
/// The pipeline owns this Interface because it is the consumer. Runtime
/// composition supplies the concrete Adapters, while the query implementation
/// only relies on these behavioral capabilities.
#[derive(Clone)]
pub(crate) struct QueryPorts {
    pub(crate) embedder: Arc<dyn Embedder>,
    pub(crate) knowledge: Arc<dyn KnowledgeStore>,
    pub(crate) llm: Arc<dyn Llm>,
}

pub use super::synthesis::QueryResponse;

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
    /// The control plane could not hydrate synthesis metadata.
    #[error("query control store: {0}")]
    Control(#[from] crate::control::DbError),
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
    validate_query(&config, query_text, top_k)?;
    let ports = crate::runtime::query_ports(&config)?;
    Ok(retrieve_context(config, ports, query_text, top_k)
        .await?
        .context
        .chunks)
}

/// Runs retrieval and then attempts bounded, citation-preserving LLM synthesis.
///
/// Synthesis failures are deliberately values at this interactive boundary:
/// the ranked chunks are returned when the endpoint is unavailable, rate
/// limited, or returns malformed/ungrounded JSON. Retrieval failures still
/// propagate because they indicate a broken required path.
///
/// # Errors
/// [`QueryError`] for invalid input, provider startup, retrieval, or control
/// metadata failures needed to render the synthesis context.
pub async fn answer(
    config: Config,
    query_text: &str,
    top_k: usize,
) -> Result<QueryResponse, QueryError> {
    validate_query(&config, query_text, top_k)?;
    let ports = crate::runtime::query_ports(&config)?;
    answer_with_ports(config, ports, query_text, top_k).await
}

/// Runs operator retrieval with an already assembled set of ports.
///
/// Runtime composition uses this Interface for long-lived HTTP processes so
/// successful provider construction is reused across requests. The public
/// [`answer`] helper remains available for one-shot CLI callers.
pub(crate) async fn answer_with_ports(
    config: Config,
    ports: QueryPorts,
    query_text: &str,
    top_k: usize,
) -> Result<QueryResponse, QueryError> {
    let session = retrieve_context(config, ports, query_text, top_k).await?;
    synthesis::run(
        &session.config,
        &session.conn,
        session.ports.llm.as_ref(),
        query_text,
        session.context,
    )
    .await
    .map_err(QueryError::Control)
}

struct QuerySession {
    config: Arc<Config>,
    conn: ControlDb,
    ports: QueryPorts,
    context: RetrievedContext,
}

async fn retrieve_context(
    config: Config,
    ports: QueryPorts,
    query_text: &str,
    top_k: usize,
) -> Result<QuerySession, QueryError> {
    validate_query(&config, query_text, top_k)?;
    let config = Arc::new(config);
    std::fs::create_dir_all(config.data_dir())?;
    if let Some(parent) = config.db_path().parent() {
        std::fs::create_dir_all(parent)?;
    }

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
    let context = retriever
        .query_context(query_text, top_k)
        .await
        .map_err(QueryError::Retrieve)?;
    Ok(QuerySession {
        config,
        conn,
        ports,
        context,
    })
}

/// Validates the operator query before runtime Adapters are initialized.
pub(crate) fn validate_query(
    config: &Config,
    query_text: &str,
    top_k: usize,
) -> Result<(), QueryError> {
    if query_text.trim().is_empty() {
        return Err(QueryError::Empty);
    }
    if top_k == 0 {
        return Err(QueryError::InvalidTopK);
    }
    if top_k > config.retrieval().pool() {
        return Err(QueryError::TopKExceedsPool {
            requested: top_k,
            pool: config.retrieval().pool(),
        });
    }
    Ok(())
}
