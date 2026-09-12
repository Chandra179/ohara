//! Metrics endpoint and its transport representation.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use super::AppState;
use super::error::ApiError;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MetricsResponse {
    captured_at: String,
    documents_by_status: BTreeMap<String, u64>,
    due_for_recrawl: u64,
    events_by_outcome: BTreeMap<String, u64>,
    events_by_stage: BTreeMap<String, u64>,
    jobs_by_stage_status: BTreeMap<String, BTreeMap<String, u64>>,
    llm_usage: crate::control::LlmUsageSnapshot,
    pending_er_reviews: u64,
    raw_bytes: u64,
    raw_files: u64,
    raw_max_age_days: Option<u64>,
    raw_max_bytes: Option<u64>,
}

impl From<crate::ops::MetricsReport> for MetricsResponse {
    fn from(report: crate::ops::MetricsReport) -> Self {
        let control = report.control;
        Self {
            captured_at: control.captured_at,
            documents_by_status: control.documents_by_status,
            due_for_recrawl: control.due_for_recrawl,
            events_by_outcome: control.events_by_outcome,
            events_by_stage: control.events_by_stage,
            jobs_by_stage_status: control.jobs_by_stage_status,
            llm_usage: control.llm_usage,
            pending_er_reviews: control.pending_er_reviews,
            raw_bytes: report.raw_bytes,
            raw_files: report.raw_files,
            raw_max_age_days: report.raw_max_age_days,
            raw_max_bytes: report.raw_max_bytes,
        }
    }
}

/// Serves the read-only operator metrics snapshot.
pub(super) async fn metrics(
    State(state): State<Arc<AppState>>,
) -> Result<Json<MetricsResponse>, ApiError> {
    let config = state.config.clone();
    let report = tokio::task::spawn_blocking(move || crate::ops::metrics_read_only(&config))
        .await
        .map_err(|error| ApiError::internal(format!("metrics task failed: {error}")))?
        .map_err(|error| ApiError::service_unavailable(&error))?;
    Ok(Json(report.into()))
}
