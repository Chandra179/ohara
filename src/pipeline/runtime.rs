//! Provider and runtime assembly for the worker and operator query paths.
//!
//! This module owns the construction-time invariants shared by those paths:
//! feature-gated defaults, model compatibility, and provider capability checks.
//! The worker and query modules retain their different lifecycles, but they no
//! longer need to know how a default adapter is assembled.

use std::sync::Arc;

use crate::BootError;
use crate::config::Config;
use crate::engine::{FetchLadder, Fetcher, HttpFetcher, HttpFetcherParams};
use crate::knowledge::KnowledgeStore;
use crate::llm::Llm;

use super::clean::{Extractor, ReadabilityExtractor};
use super::embed::Embedder;

/// The fully assembled default provider set for a worker.
pub(crate) struct WorkerPorts {
    pub(crate) fetcher: Arc<dyn Fetcher>,
    pub(crate) extractor: Arc<dyn Extractor>,
    pub(crate) embedder: Arc<dyn Embedder>,
    pub(crate) knowledge: Arc<dyn KnowledgeStore>,
    pub(crate) llm: Arc<dyn Llm>,
}

/// The subset of default providers required by operator retrieval.
pub(crate) struct QueryPorts {
    pub(crate) embedder: Arc<dyn Embedder>,
    pub(crate) knowledge: Arc<dyn KnowledgeStore>,
    pub(crate) llm: Arc<dyn Llm>,
}

/// Assembles the default worker adapters and validates their shared contracts.
pub(crate) fn worker_ports(config: &Config) -> Result<WorkerPorts, BootError> {
    let plain = Arc::new(HttpFetcher::new(HttpFetcherParams {
        user_agent: config.fetcher().user_agent().to_string(),
        timeout: config.fetcher().timeout(),
        rate_limit: config.rate_limit(),
        allow_private_hosts: config.fetcher().allow_private_hosts(),
        max_body_bytes: config.fetcher().max_body_bytes(),
        max_redirects: config.fetcher().max_redirects(),
    })?);
    let fetcher = Arc::new(FetchLadder::single(plain));
    let extractor = Arc::new(ReadabilityExtractor);
    let embedder = default_embedder(config)?;
    validate_embedder(config, embedder.as_ref())?;
    let knowledge = default_knowledge(config)?;
    let llm = default_llm(config)?;
    Ok(WorkerPorts {
        fetcher,
        extractor,
        embedder,
        knowledge,
        llm,
    })
}

/// Assembles the default adapters required by the operator query path.
pub(crate) fn query_ports(config: &Config) -> Result<QueryPorts, BootError> {
    let embedder = default_embedder(config)?;
    validate_embedder(config, embedder.as_ref())?;
    let knowledge = default_knowledge(config)?;
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
    Ok(Arc::new(crate::llm::Ollama::new(
        config.llm().base_url().clone(),
        config.llm().health_timeout(),
    )?))
}

#[cfg(feature = "onnx-embedder")]
fn default_embedder(config: &Config) -> Result<Arc<dyn Embedder>, BootError> {
    Ok(Arc::new(super::embed::LocalEmbedder::new(
        &config.data_dir().join("models"),
    )?))
}

#[cfg(not(feature = "onnx-embedder"))]
fn default_embedder(_config: &Config) -> Result<Arc<dyn Embedder>, BootError> {
    Err(BootError::Worker(
        "ohara was built without the `onnx-embedder` feature; provide an Embedder via Worker::with_ports (§9 provider swap)".to_string(),
    ))
}

#[cfg(feature = "ladybug")]
fn default_knowledge(config: &Config) -> Result<Arc<dyn KnowledgeStore>, BootError> {
    std::fs::create_dir_all(config.data_dir()).map_err(|e| {
        BootError::Worker(format!(
            "cannot create data dir {}: {e}",
            config.data_dir().display()
        ))
    })?;
    Ok(Arc::new(crate::knowledge::LadybugStore::open(
        &config.data_dir().join("ladybug"),
        config.embedder().dim(),
    )?))
}

#[cfg(not(feature = "ladybug"))]
fn default_knowledge(_config: &Config) -> Result<Arc<dyn KnowledgeStore>, BootError> {
    Err(BootError::Worker(
        "ohara was built without the `ladybug` feature; provide a KnowledgeStore via Worker::with_ports (§9 provider swap)".to_string(),
    ))
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

fn default_llm(config: &Config) -> Result<Arc<dyn Llm>, BootError> {
    if !config.pipeline().graph_enabled() {
        return Ok(Arc::new(crate::llm::NoLlm));
    }
    let ollama = crate::llm::Ollama::new(
        config.llm().base_url().clone(),
        config.llm().health_timeout(),
    )?;
    let handle = tokio::runtime::Handle::try_current().map_err(|_| {
        BootError::Worker(
            "ohara must run inside a tokio runtime to health-check the LLM endpoint".to_string(),
        )
    })?;
    handle.block_on(ollama.verify_endpoint())?;
    Ok(Arc::new(ollama))
}

/// Acquires the process-wide runtime lock used by workers and operator actions.
pub(crate) fn acquire_lock(config: &Config) -> Result<crate::ops::RuntimeLock, BootError> {
    crate::ops::RuntimeLock::acquire(config.data_dir())
        .map_err(|error| BootError::Worker(error.to_string()))
}
