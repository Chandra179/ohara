//! Composition-root services shared by the worker and local HTTP transport.
//!
//! Runtime assembly is kept outside the pipeline so the pipeline can depend on
//! behavioral ports without naming concrete storage or network adapters.

use std::sync::{Arc, Mutex, MutexGuard};

use crate::BootError;
use crate::config::Config;

mod providers;
mod readiness;

pub(crate) use providers::{
    WorkerPorts, acquire_lock, query_ports, read_only_knowledge, topic_searcher, validate_embedder,
    worker_ports, writable_knowledge,
};
pub(crate) use readiness::{ReadinessCheck, WorkerReadiness, readiness};

/// Process-local owner for the operator query Adapters.
///
/// Query provider construction is lazy so the API can start and expose an
/// actionable readiness diagnostic when a model or knowledge service is
/// unavailable. Expensive model and client providers are retained and cloned
/// by each request; the read-only knowledge handle is request-scoped so worker
/// commits become visible without restarting the API.
pub(crate) struct QueryRuntime {
    providers: Mutex<Option<QueryProviderCache>>,
}

struct QueryProviderCache {
    embedder: Arc<dyn crate::pipeline::Embedder>,
    llm: Arc<dyn crate::llm::Llm>,
}

impl Default for QueryRuntime {
    fn default() -> Self {
        Self {
            providers: Mutex::new(None),
        }
    }
}

impl QueryRuntime {
    /// Returns query ports after readiness permits them. The model and LLM
    /// providers are cached because they are expensive to construct; the
    /// knowledge client is reopened for each request so service changes and
    /// worker commits are visible to the API.
    pub(crate) fn ports(&self, config: &Config) -> Result<crate::pipeline::QueryPorts, BootError> {
        let cached = {
            let guard = self.lock_providers();
            guard
                .as_ref()
                .map(|providers| (Arc::clone(&providers.embedder), Arc::clone(&providers.llm)))
        };
        if let Some((embedder, llm)) = cached {
            return Ok(crate::pipeline::QueryPorts {
                embedder,
                knowledge: read_only_knowledge(config)?,
                llm,
            });
        }

        let ports = query_ports(config)?;
        let providers = QueryProviderCache {
            embedder: Arc::clone(&ports.embedder),
            llm: Arc::clone(&ports.llm),
        };
        *self.lock_providers() = Some(providers);
        Ok(ports)
    }

    fn lock_providers(&self) -> MutexGuard<'_, Option<QueryProviderCache>> {
        self.providers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
