//! Default adapter assembly for the local runtime.
//!
//! This is the composition root for concrete providers. It is the only place
//! that combines pipeline ports with Engine, Control, and Knowledge adapters.

use std::sync::Arc;

use crate::BootError;
use crate::config::Config;
use crate::engine::Ollama;
use crate::engine::{
    BingNewsSearcher, FetchLadder, FetchLeg, Fetcher, HttpFetcher, HttpFetcherParams,
    ImpersonationFetcher, ObscuraFetcher, TopicSearcher,
};
use crate::knowledge::KnowledgeStore;
use crate::llm::Llm;
use crate::pipeline::{Embedder, Extractor, QueryPorts, ReadabilityExtractor};

/// The fully assembled default provider set for a worker.
pub(crate) struct WorkerPorts {
    pub(crate) fetcher: Arc<dyn Fetcher>,
    pub(crate) extractor: Arc<dyn Extractor>,
    pub(crate) embedder: Arc<dyn Embedder>,
    pub(crate) knowledge: Arc<dyn KnowledgeStore>,
    pub(crate) llm: Arc<dyn Llm>,
}

/// Assembles the network-owned topic search adapter for the local API.
pub(crate) fn topic_searcher(config: &Config) -> Arc<dyn TopicSearcher> {
    Arc::new(BingNewsSearcher::new(
        config.fetcher().user_agent().to_string(),
        config.fetcher().timeout(),
    ))
}

/// Assembles the default worker adapters and validates their shared contracts.
pub(crate) async fn worker_ports(config: &Config) -> Result<WorkerPorts, BootError> {
    let params = http_params(config);
    let plain = Arc::new(HttpFetcher::new(params.clone())?);
    let impersonated = Arc::new(ImpersonationFetcher::new(
        params.clone(),
        config.fetcher().impersonation_user_agent().to_string(),
    )?);
    let mut legs: Vec<(FetchLeg, Arc<dyn Fetcher>)> = vec![
        (FetchLeg::Plain, plain),
        (FetchLeg::Impersonate, impersonated),
    ];
    if let Some(command) = config.fetcher().obscura_command() {
        legs.push((
            FetchLeg::Browser,
            Arc::new(ObscuraFetcher::new(command.to_path_buf(), params)?),
        ));
    }
    let fetcher = Arc::new(FetchLadder::new(legs)?);
    let extractor = Arc::new(ReadabilityExtractor);
    let embedder = default_embedder(config)?;
    validate_embedder(config, embedder.as_ref())?;
    let knowledge = default_knowledge(config)?;
    let llm = default_llm(config).await?;
    Ok(WorkerPorts {
        fetcher,
        extractor,
        embedder,
        knowledge,
        llm,
    })
}

fn http_params(config: &Config) -> HttpFetcherParams {
    HttpFetcherParams {
        user_agent: config.fetcher().user_agent().to_string(),
        timeout: config.fetcher().timeout(),
        rate_limit: config.rate_limit(),
        allow_private_hosts: config.fetcher().allow_private_hosts(),
        max_body_bytes: config.fetcher().max_body_bytes(),
        max_redirects: config.fetcher().max_redirects(),
    }
}

/// Assembles the default adapters required by the operator query path.
pub(crate) fn query_ports(config: &Config) -> Result<QueryPorts, BootError> {
    // The API query path must fail fast when the model is absent. Health has
    // already reported the actionable download instruction; starting a second
    // network download from a request would leave the browser waiting on a
    // provider operation with no useful progress state.
    crate::pipeline::check_embedder_readiness(config)?;
    let embedder = default_embedder(config)?;
    validate_embedder(config, embedder.as_ref())?;
    let knowledge = read_only_knowledge(config)?;
    let llm = query_llm(config)?;
    Ok(QueryPorts {
        embedder,
        knowledge,
        llm,
    })
}

/// Assembles the query LLM without the worker's boot health gate. Retrieval
/// remains useful while Ollama is stopped; synthesis then degrades to ranked
/// chunks at the query boundary (§8 Stage 5.6).
fn query_llm(config: &Config) -> Result<Arc<dyn Llm>, BootError> {
    Ok(Arc::new(Ollama::new(
        config.llm().base_url().clone(),
        config.llm().health_timeout(),
    )?))
}

#[cfg(feature = "onnx-embedder")]
fn default_embedder(config: &Config) -> Result<Arc<dyn Embedder>, BootError> {
    Ok(Arc::new(crate::pipeline::LocalEmbedder::new(
        &config.data_dir().join("models"),
    )?))
}

#[cfg(not(feature = "onnx-embedder"))]
fn default_embedder(_config: &Config) -> Result<Arc<dyn Embedder>, BootError> {
    Err(BootError::Worker(
        "ohara was built without the `onnx-embedder` feature; provide an Embedder via Worker::with_ports (§9 provider swap)".to_string(),
    ))
}

fn default_knowledge(config: &Config) -> Result<Arc<dyn KnowledgeStore>, BootError> {
    Ok(Arc::new(crate::knowledge::RemoteKnowledgeStore::connect(
        config.knowledge(),
        config.embedder().dim(),
        false,
    )?))
}

pub(crate) fn writable_knowledge(config: &Config) -> Result<Arc<dyn KnowledgeStore>, BootError> {
    default_knowledge(config)
}

pub(crate) fn read_only_knowledge(config: &Config) -> Result<Arc<dyn KnowledgeStore>, BootError> {
    Ok(Arc::new(crate::knowledge::RemoteKnowledgeStore::connect(
        config.knowledge(),
        config.embedder().dim(),
        true,
    )?))
}

/// Validates the model and input-capacity contracts shared by the configured
/// namespace and an injected or default embedder.
pub(crate) fn validate_embedder(config: &Config, embedder: &dyn Embedder) -> Result<(), BootError> {
    if embedder.model_id() != config.embedder().model_id() {
        return Err(BootError::Worker(format!(
            "embedder model {:?} does not match configured embedder.model_id {:?}",
            embedder.model_id(),
            config.embedder().model_id()
        )));
    }
    if embedder.dim() != config.embedder().dim() {
        return Err(BootError::Worker(format!(
            "embedder dimension {} does not match configured embedder.dim {}",
            embedder.dim(),
            config.embedder().dim()
        )));
    }
    if config.pipeline().chunk_budget_tokens() > embedder.max_input_tokens() {
        return Err(BootError::Worker(format!(
            "pipeline chunk budget {} exceeds embedder max input tokens {}",
            config.pipeline().chunk_budget_tokens(),
            embedder.max_input_tokens()
        )));
    }
    if embedder.model_id() != config.knowledge().write_model() {
        return Err(BootError::Worker(format!(
            "embedder model {:?} does not match knowledge.write_model {:?}",
            embedder.model_id(),
            config.knowledge().write_model()
        )));
    }
    Ok(())
}

async fn default_llm(config: &Config) -> Result<Arc<dyn Llm>, BootError> {
    if !config.pipeline().graph_enabled() {
        return Ok(Arc::new(crate::llm::NoLlm));
    }
    let ollama = Ollama::new(
        config.llm().base_url().clone(),
        config.llm().health_timeout(),
    )?;
    ollama.verify_endpoint().await?;
    Ok(Arc::new(ollama))
}

/// Acquires the process-wide runtime lock used by workers and operator actions.
pub(crate) fn acquire_lock(config: &Config) -> Result<crate::ops::RuntimeLock, BootError> {
    crate::ops::RuntimeLock::acquire(config.data_dir())
        .map_err(|error| BootError::Worker(error.to_string()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn default_llm_health_check_does_not_block_the_runtime() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("ohara.toml");
        std::fs::write(
            &config_path,
            format!(
                "data_dir = {:?}\n[pipeline]\ngraph_enabled = true\n[llm]\nbase_url = \"http://127.0.0.1:9\"\nhealth_timeout_secs = 1\n",
                directory.path()
            ),
        )
        .unwrap();
        let config = Config::load(Some(&config_path)).unwrap();

        let result = default_llm(&config).await;

        assert!(matches!(
            result,
            Err(BootError::Llm(crate::llm::LlmError::Unavailable(_)))
        ));
    }
}
