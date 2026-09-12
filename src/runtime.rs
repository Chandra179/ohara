//! Composition-root services shared by the worker and local HTTP transport.
//!
//! Runtime assembly is kept outside the pipeline so the pipeline can depend on
//! behavioral ports without naming concrete storage or network adapters.

mod providers;
mod readiness;

pub(crate) use providers::{
    QueryPorts, WorkerPorts, acquire_lock, query_ports, validate_embedder, worker_ports,
};
pub(crate) use readiness::{ReadinessCheck, readiness};
