//! Overview endpoint and its transport representation.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use super::AppState;
use super::error::ApiError;

const OVERVIEW_QUEUE_LIMIT: usize = 10;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OverviewResponse {
    documents_by_status: BTreeMap<String, u64>,
    queue: Vec<QueueResponse>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QueueResponse {
    job_id: String,
    document_id: String,
    title: Option<String>,
    source_url: String,
    document_status: String,
    stage: String,
    job_status: String,
    updated_at: String,
    error: Option<String>,
}

impl From<crate::control::QueueItem> for QueueResponse {
    fn from(item: crate::control::QueueItem) -> Self {
        Self {
            job_id: item.job_id,
            document_id: item.doc_id,
            title: item.title,
            source_url: item.source_url,
            document_status: item.document_status,
            stage: item.stage,
            job_status: item.job_status,
            updated_at: item.updated_at,
            error: item.error,
        }
    }
}

/// Serves the document counts and bounded queue projection for the overview.
pub(super) async fn overview(
    State(state): State<Arc<AppState>>,
) -> Result<Json<OverviewResponse>, ApiError> {
    let config = state.config.clone();
    let snapshot = tokio::task::spawn_blocking(move || {
        let db = crate::control::connect(config.db_path())?;
        crate::control::overview(&db, OVERVIEW_QUEUE_LIMIT)
    })
    .await
    .map_err(|error| ApiError::internal(format!("overview task failed: {error}")))?
    .map_err(|error| ApiError::control(&error))?;
    Ok(Json(OverviewResponse {
        documents_by_status: snapshot.documents_by_status,
        queue: snapshot.queue.into_iter().map(Into::into).collect(),
    }))
}
