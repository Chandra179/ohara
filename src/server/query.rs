//! Query endpoint and its transport representation.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};

use super::AppState;
use super::error::ApiError;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct QueryRequest {
    query: String,
    top_k: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct QueryResponse {
    answer: Option<String>,
    citations: Vec<String>,
    chunks: Vec<ScoredChunkResponse>,
    availability: crate::pipeline::QueryAvailability,
    grounding: crate::pipeline::QueryGrounding,
    reranker: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScoredChunkResponse {
    chunk_id: String,
    score: f32,
    text: String,
}

impl From<crate::pipeline::ScoredChunk> for ScoredChunkResponse {
    fn from(chunk: crate::pipeline::ScoredChunk) -> Self {
        Self {
            chunk_id: chunk.chunk_id,
            score: chunk.score,
            text: chunk.text,
        }
    }
}

/// Runs the operator query and maps the provider-neutral result to JSON.
pub(super) async fn query(
    State(state): State<Arc<AppState>>,
    Json(request): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, ApiError> {
    let top_k = request
        .top_k
        .unwrap_or_else(|| state.config.retrieval().top_k());
    let config = state.config.clone();
    let query_text = request.query;
    let handle = tokio::runtime::Handle::current();
    let response = tokio::task::spawn_blocking(move || {
        handle
            .block_on(crate::pipeline::answer(config, &query_text, top_k))
            .map_err(|error| ApiError::query(&error))
    })
    .await
    .map_err(|error| ApiError::internal(format!("query task failed: {error}")))??;
    Ok(Json(QueryResponse {
        answer: response.answer,
        citations: response.citations,
        chunks: response.chunks.into_iter().map(Into::into).collect(),
        availability: response.availability,
        grounding: response.grounding,
        reranker: "identity",
    }))
}
