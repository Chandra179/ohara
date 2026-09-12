//! Transport error mapping for the local HTTP interface.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::ops;
use crate::pipeline;

/// Maps internal failures to the stable local HTTP error shape.
pub(super) struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub(super) fn bad_request(message: String) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message,
        }
    }

    pub(super) fn not_found(message: String) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message,
        }
    }

    pub(super) fn internal(message: String) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message,
        }
    }

    pub(super) fn query(error: &pipeline::QueryError) -> Self {
        let status = match error {
            pipeline::QueryError::Empty
            | pipeline::QueryError::InvalidTopK
            | pipeline::QueryError::TopKExceedsPool { .. } => StatusCode::BAD_REQUEST,
            pipeline::QueryError::Io(_)
            | pipeline::QueryError::Boot(_)
            | pipeline::QueryError::Retrieve(_)
            | pipeline::QueryError::Control(_) => StatusCode::SERVICE_UNAVAILABLE,
        };
        Self {
            status,
            message: error.to_string(),
        }
    }

    pub(super) fn service_unavailable(error: &ops::OpsError) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: error.to_string(),
        }
    }

    pub(super) fn control(error: &crate::control::DbError) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: format!("control store unavailable: {error}"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}
