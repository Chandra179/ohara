//! Topic discovery endpoint.
//!
//! This transport module validates the small browser request, delegates search
//! to the Engine port, and registers results through the Control facade. It
//! does not know RSS, SQL, or the worker implementation.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};

use super::AppState;
use super::error::ApiError;

const DEFAULT_RESULT_LIMIT: usize = 5;
const MAX_RESULT_LIMIT: usize = 10;
const MAX_TOPIC_CHARS: usize = 200;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ScrapeTopicRequest {
    topic: String,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ScrapeTopicResponse {
    topic: String,
    requested: usize,
    discovered: usize,
    enqueued: usize,
    duplicates: usize,
    documents: Vec<QueuedTopicDocument>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QueuedTopicDocument {
    title: String,
    source_url: String,
    document_id: String,
    job_id: Option<String>,
    status: &'static str,
}

/// Searches a topic and queues the discovered article URLs for the worker.
pub(super) async fn scrape_topic(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ScrapeTopicRequest>,
) -> Result<Json<ScrapeTopicResponse>, ApiError> {
    let topic = request.topic.trim().to_string();
    if topic.is_empty() {
        return Err(ApiError::bad_request("topic must not be empty".to_string()));
    }
    if topic.chars().count() > MAX_TOPIC_CHARS {
        return Err(ApiError::bad_request(format!(
            "topic must be at most {MAX_TOPIC_CHARS} characters"
        )));
    }
    let limit = request.limit.unwrap_or(DEFAULT_RESULT_LIMIT);
    if !(1..=MAX_RESULT_LIMIT).contains(&limit) {
        return Err(ApiError::bad_request(format!(
            "topic result limit must be between 1 and {MAX_RESULT_LIMIT}"
        )));
    }

    let discovered = state
        .topic_searcher
        .search(&topic, limit)
        .await
        .map_err(|error| ApiError::topic_search(&error))?;
    let discovered_count = discovered.len();
    let config = state.config.clone();
    let queued = tokio::task::spawn_blocking(move || {
        let db = crate::control::connect(config.db_path())?;
        let now = crate::control::now_stamp();
        discovered
            .into_iter()
            .map(|result| {
                let source_url = result.url.as_str().to_string();
                let outcome = crate::control::insert_new(
                    &db,
                    config.data_dir(),
                    &crate::control::NewDocument {
                        source_url: source_url.clone(),
                        source_url_normalized: source_url.clone(),
                        priority: crate::control::DEFAULT_JOB_PRIORITY,
                        pipeline_version: env!("CARGO_PKG_VERSION").to_string(),
                    },
                    &now,
                )?;
                Ok::<_, crate::control::DbError>((result.title, source_url, outcome))
            })
            .collect::<Result<Vec<_>, _>>()
    })
    .await
    .map_err(|error| ApiError::internal(format!("topic queue task failed: {error}")))?
    .map_err(|error| ApiError::control(&error))?;

    let mut enqueued = 0;
    let mut duplicates = 0;
    let documents = queued
        .into_iter()
        .map(|(title, source_url, outcome)| match outcome {
            crate::control::EnqueueOutcome::Enqueued { doc_id, job_id } => {
                enqueued += 1;
                QueuedTopicDocument {
                    title,
                    source_url,
                    document_id: doc_id,
                    job_id: Some(job_id),
                    status: "enqueued",
                }
            }
            crate::control::EnqueueOutcome::Duplicate { doc_id } => {
                duplicates += 1;
                QueuedTopicDocument {
                    title,
                    source_url,
                    document_id: doc_id,
                    job_id: None,
                    status: "duplicate",
                }
            }
        })
        .collect();

    Ok(Json(ScrapeTopicResponse {
        topic,
        requested: limit,
        discovered: discovered_count,
        enqueued,
        duplicates,
        documents,
    }))
}
