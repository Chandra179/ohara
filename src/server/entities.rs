//! Entity-review endpoints and their transport representations.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use serde::Serialize;

use super::AppState;
use super::error::ApiError;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EntitySummaryResponse {
    aliases: u64,
    id: String,
    name: String,
    #[serde(rename = "type")]
    entity_type: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct EntityReviewResponse {
    id: String,
    candidate_a: EntitySummaryResponse,
    candidate_b: EntitySummaryResponse,
    score: Option<f64>,
}

impl From<crate::control::EntityReviewItem> for EntityReviewResponse {
    fn from(item: crate::control::EntityReviewItem) -> Self {
        Self {
            id: item.review_id.to_string(),
            candidate_a: item.entity_a.into(),
            candidate_b: item.entity_b.into(),
            score: item.score,
        }
    }
}

impl From<crate::control::EntitySummary> for EntitySummaryResponse {
    fn from(entity: crate::control::EntitySummary) -> Self {
        Self {
            aliases: entity.alias_count,
            id: entity.entity_id,
            name: entity.canonical_name,
            entity_type: entity.entity_type,
        }
    }
}

/// Serves pending entity-resolution review candidates.
pub(super) async fn entity_reviews(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<EntityReviewResponse>>, ApiError> {
    let config = state.config.clone();
    let reviews = tokio::task::spawn_blocking(move || {
        let db = crate::control::connect(config.db_path())?;
        crate::control::entity_review_items(&db)
    })
    .await
    .map_err(|error| ApiError::internal(format!("entity reviews task failed: {error}")))?
    .map_err(|error| ApiError::control(&error))?;
    Ok(Json(reviews.into_iter().map(Into::into).collect()))
}

/// Serves a single hydrated entity-review preview.
pub(super) async fn entity_review_preview(
    State(state): State<Arc<AppState>>,
    Path(review_id): Path<String>,
) -> Result<Json<EntityReviewResponse>, ApiError> {
    let review_id = review_id.parse::<i64>().map_err(|_| {
        ApiError::bad_request("entity review id must be a positive integer".to_string())
    })?;
    if review_id < 1 {
        return Err(ApiError::bad_request(
            "entity review id must be a positive integer".to_string(),
        ));
    }
    let config = state.config.clone();
    let review = tokio::task::spawn_blocking(move || {
        let db = crate::control::connect(config.db_path())?;
        crate::control::entity_review_items(&db).map(|reviews| {
            reviews
                .into_iter()
                .find(|review| review.review_id == review_id)
        })
    })
    .await
    .map_err(|error| ApiError::internal(format!("entity preview task failed: {error}")))?
    .map_err(|error| ApiError::control(&error))?
    .ok_or_else(|| ApiError::not_found(format!("entity review {review_id} was not found")))?;
    Ok(Json(review.into()))
}
