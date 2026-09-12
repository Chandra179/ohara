//! Document-list endpoint and its transport representation.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use serde::{Deserialize, Serialize};

use super::AppState;
use super::error::ApiError;

const DEFAULT_PAGE_SIZE: usize = 25;
const MAX_PAGE_SIZE: usize = crate::control::MAX_DOCUMENT_PAGE_SIZE;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct DocumentRequest {
    limit: Option<usize>,
    cursor: Option<String>,
    status: Option<String>,
    search: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DocumentPageResponse {
    items: Vec<DocumentResponse>,
    next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DocumentResponse {
    id: String,
    source_url: String,
    title: Option<String>,
    status: String,
    chunk_count: u64,
    created_at: String,
    last_processed_at: Option<String>,
    error: Option<String>,
}

impl From<crate::control::DocumentListItem> for DocumentResponse {
    fn from(item: crate::control::DocumentListItem) -> Self {
        Self {
            id: item.doc_id,
            source_url: item.source_url,
            title: item.title,
            status: item.status,
            chunk_count: item.chunk_count,
            created_at: item.created_at,
            last_processed_at: item.last_processed_at,
            error: item.error,
        }
    }
}

fn document_query(request: DocumentRequest) -> Result<crate::control::DocumentListQuery, ApiError> {
    let limit = request.limit.unwrap_or(DEFAULT_PAGE_SIZE);
    if !(1..=MAX_PAGE_SIZE).contains(&limit) {
        return Err(ApiError::bad_request(format!(
            "document limit must be between 1 and {MAX_PAGE_SIZE}"
        )));
    }
    let status = request.status.map(|value| {
        value
            .parse::<crate::control::DocStatus>()
            .map_err(|()| ApiError::bad_request(format!("unknown document status {value:?}")))
    });
    let status = status.transpose()?;
    let cursor = request.cursor.filter(|value| !value.trim().is_empty());
    let search = request.search.and_then(|value| {
        let value = value.trim().to_string();
        (!value.is_empty()).then_some(value)
    });
    Ok(crate::control::DocumentListQuery {
        limit,
        cursor,
        status,
        search,
    })
}

/// Serves a bounded, cursor-paginated document page.
pub(super) async fn documents(
    State(state): State<Arc<AppState>>,
    Query(request): Query<DocumentRequest>,
) -> Result<Json<DocumentPageResponse>, ApiError> {
    let query = document_query(request)?;
    let config = state.config.clone();
    let page = tokio::task::spawn_blocking(move || {
        let db = crate::control::connect(config.db_path())?;
        crate::control::list_documents(&db, &query)
    })
    .await
    .map_err(|error| ApiError::internal(format!("documents task failed: {error}")))?
    .map_err(|error| ApiError::control(&error))?;
    Ok(Json(DocumentPageResponse {
        items: page.items.into_iter().map(Into::into).collect(),
        next_cursor: page.next_cursor,
    }))
}
