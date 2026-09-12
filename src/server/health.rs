//! HTTP health representation over the shared runtime readiness report.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use super::AppState;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceStatus {
    Healthy,
    Degraded,
    Offline,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ComponentStatus {
    Available,
    Unavailable,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct HealthResponse {
    status: ServiceStatus,
    control_store: ComponentStatus,
    knowledge_store: ComponentStatus,
    embedder: ComponentStatus,
    llm: ComponentStatus,
    worker: WorkerResponse,
    reranker: &'static str,
    diagnostics: Vec<ReadinessDiagnostic>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkerResponse {
    status: ComponentStatus,
    state: Option<crate::control::WorkerState>,
    worker_id: Option<String>,
    process_id: Option<u32>,
    started_at: Option<String>,
    last_heartbeat_at: Option<String>,
    current_stage: Option<String>,
    current_job_id: Option<String>,
    last_error: Option<String>,
    stale: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReadinessDiagnostic {
    component: &'static str,
    message: String,
    action: String,
}

/// Serves the shared readiness report as the public health contract.
pub(super) async fn health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let report = crate::runtime::readiness(state.config.clone()).await;
    let control_store = status(&report.control_store);
    let knowledge_store = status(&report.knowledge_store);
    let embedder = status(&report.embedder);
    let llm = status(&report.llm);
    let mut diagnostics: Vec<ReadinessDiagnostic> = [
        &report.control_store,
        &report.knowledge_store,
        &report.embedder,
        &report.llm,
    ]
    .into_iter()
    .filter_map(|check| check.diagnostic.as_ref())
    .map(|diagnostic| ReadinessDiagnostic {
        component: diagnostic.component,
        message: diagnostic.message.clone(),
        action: diagnostic.action.clone(),
    })
    .collect();
    if let Some(diagnostic) = report.worker.diagnostic.as_ref() {
        diagnostics.push(ReadinessDiagnostic {
            component: diagnostic.component,
            message: diagnostic.message.clone(),
            action: diagnostic.action.clone(),
        });
    }
    Json(HealthResponse {
        status: overall_status(
            control_store,
            knowledge_store,
            embedder,
            llm,
            status_worker(&report.worker),
        ),
        control_store,
        knowledge_store,
        embedder,
        llm,
        worker: worker_response(&report.worker),
        reranker: "identity",
        diagnostics,
    })
}

fn status_worker(check: &crate::runtime::WorkerReadiness) -> ComponentStatus {
    if check.available {
        ComponentStatus::Available
    } else {
        ComponentStatus::Unavailable
    }
}

fn worker_response(check: &crate::runtime::WorkerReadiness) -> WorkerResponse {
    let observation = check.observation.as_ref();
    WorkerResponse {
        status: status_worker(check),
        state: observation.map(|item| item.status.state),
        worker_id: observation.map(|item| item.status.worker_id.clone()),
        process_id: observation.map(|item| item.status.process_id),
        started_at: observation.map(|item| item.status.started_at.clone()),
        last_heartbeat_at: observation.map(|item| item.status.last_heartbeat_at.clone()),
        current_stage: observation.and_then(|item| item.status.current_stage.clone()),
        current_job_id: observation.and_then(|item| item.status.current_job_id.clone()),
        last_error: observation.and_then(|item| item.status.last_error.clone()),
        stale: observation.is_some_and(|item| item.stale),
    }
}

fn status(check: &crate::runtime::ReadinessCheck) -> ComponentStatus {
    if check.available {
        ComponentStatus::Available
    } else {
        ComponentStatus::Unavailable
    }
}

pub const fn overall_status(
    control_store: ComponentStatus,
    knowledge_store: ComponentStatus,
    embedder: ComponentStatus,
    llm: ComponentStatus,
    worker: ComponentStatus,
) -> ServiceStatus {
    if matches!(control_store, ComponentStatus::Unavailable)
        || matches!(knowledge_store, ComponentStatus::Unavailable)
        || matches!(embedder, ComponentStatus::Unavailable)
    {
        ServiceStatus::Offline
    } else if matches!(llm, ComponentStatus::Unavailable)
        || matches!(worker, ComponentStatus::Unavailable)
    {
        ServiceStatus::Degraded
    } else {
        ServiceStatus::Healthy
    }
}
