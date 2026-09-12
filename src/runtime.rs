//! Composition-root services shared by the worker and local HTTP transport.
//!
//! Runtime assembly is kept outside the pipeline so the pipeline can depend on
//! behavioral ports without naming concrete storage or network adapters.

use std::sync::{Mutex, MutexGuard};

use crate::BootError;
use crate::config::Config;

mod providers;
mod readiness;

pub(crate) use providers::{
    WorkerPorts, acquire_lock, query_ports, topic_searcher, validate_embedder, worker_ports,
};
pub(crate) use readiness::{ReadinessCheck, WorkerReadiness, readiness};

/// Process-local owner for the operator query Adapters.
///
/// Query provider construction is lazy so the API can start and expose an
/// actionable readiness diagnostic when a model or knowledge artifact is
/// unavailable. A successful construction is retained and cloned by each
/// request, keeping model loading, store setup, and client allocation local to
/// this Module rather than repeating them in the transport.
pub(crate) struct QueryRuntime {
    ports: Mutex<Option<crate::pipeline::QueryPorts>>,
}

impl Default for QueryRuntime {
    fn default() -> Self {
        Self {
            ports: Mutex::new(None),
        }
    }
}

impl QueryRuntime {
    /// Returns the cached query ports or assembles them once after readiness
    /// permits it. Failed construction is not cached so a repaired local model
    /// or knowledge store can be retried without restarting the API.
    pub(crate) fn ports(&self, config: &Config) -> Result<crate::pipeline::QueryPorts, BootError> {
        let mut cached = self.lock_ports();
        if let Some(ports) = cached.as_ref() {
            return Ok(ports.clone());
        }
        let ports = query_ports(config)?;
        *cached = Some(ports.clone());
        Ok(ports)
    }

    fn lock_ports(&self) -> MutexGuard<'_, Option<crate::pipeline::QueryPorts>> {
        self.ports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
