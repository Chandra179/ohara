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
    reranker: &'static str,
    diagnostics: Vec<ReadinessDiagnostic>,
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
    let diagnostics = [
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
    Json(HealthResponse {
        status: overall_status(control_store, knowledge_store, embedder, llm),
        control_store,
        knowledge_store,
        embedder,
        llm,
        reranker: "identity",
        diagnostics,
    })
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
) -> ServiceStatus {
    if matches!(control_store, ComponentStatus::Unavailable)
        || matches!(knowledge_store, ComponentStatus::Unavailable)
        || matches!(embedder, ComponentStatus::Unavailable)
    {
        ServiceStatus::Offline
    } else if matches!(llm, ComponentStatus::Unavailable) {
        ServiceStatus::Degraded
    } else {
        ServiceStatus::Healthy
    }
}
