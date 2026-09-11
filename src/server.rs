//! Local HTTP transport for the frontend operator surface.
//!
//! The server is an optional process boundary. It translates JSON requests into
//! calls to the existing pipeline and operator facades; datastore access stays
//! behind those facades.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use crate::config::Config;
use crate::ops;
use crate::pipeline::{self, ScoredChunk};

/// Runs the optional local UI API until Ctrl-C.
///
/// The listener is supplied by the CLI and should normally be bound to
/// `127.0.0.1`. The server does not start the ingestion worker; run the worker
/// separately when ingestion is required.
///
/// # Errors
/// Returns [`ServerError::NonLoopbackBind`] for a non-loopback address, or
/// [`ServerError::Io`] if runtime directories or the listener cannot be created.
pub async fn run(config: Config, bind: SocketAddr) -> Result<(), ServerError> {
    if !bind.ip().is_loopback() {
        return Err(ServerError::NonLoopbackBind { bind });
    }

    tokio::fs::create_dir_all(config.data_dir()).await?;
    if let Some(parent) = config.db_path().parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let listener = TcpListener::bind(bind).await?;
    eprintln!("ohara: API server listening on http://{bind}");
    axum::serve(listener, router(config))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Server startup and shutdown failures.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// The API cannot be exposed beyond the local machine without an explicit
    /// authenticated deployment boundary.
    #[error("server must bind to a loopback address, got {bind}")]
    NonLoopbackBind {
        /// Requested listener address.
        bind: SocketAddr,
    },
    /// Runtime directories or the TCP listener could not be prepared.
    #[error("server I/O: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Clone)]
struct AppState {
    config: Config,
}

fn router(config: Config) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/metrics", get(metrics))
        .route("/api/query", post(query))
        .with_state(Arc::new(AppState { config }))
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum ServiceStatus {
    Healthy,
    Degraded,
    Offline,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum ComponentStatus {
    Available,
    Unavailable,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthResponse {
    status: ServiceStatus,
    control_store: ComponentStatus,
    knowledge_store: ComponentStatus,
    llm: ComponentStatus,
}

async fn health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let control_store = check_control_store(state.config.clone()).await;
    let knowledge_store = check_knowledge_store(state.config.clone()).await;
    let llm = check_llm(state.config.clone()).await;
    let status = overall_status(control_store, knowledge_store, llm);
    Json(HealthResponse {
        status,
        control_store,
        knowledge_store,
        llm,
    })
}

async fn check_control_store(config: Config) -> ComponentStatus {
    let result =
        tokio::task::spawn_blocking(move || crate::control::connect(config.db_path()).map(|_| ()))
            .await;
    match result {
        Ok(Ok(())) => ComponentStatus::Available,
        Ok(Err(_)) | Err(_) => ComponentStatus::Unavailable,
    }
}

async fn check_knowledge_store(config: Config) -> ComponentStatus {
    #[cfg(feature = "ladybug")]
    {
        let result = tokio::task::spawn_blocking(move || {
            crate::knowledge::LadybugStore::open(
                &config.data_dir().join("ladybug"),
                config.embedder().dim(),
            )
            .map(|_| ())
        })
        .await;
        match result {
            Ok(Ok(())) => ComponentStatus::Available,
            Ok(Err(_)) | Err(_) => ComponentStatus::Unavailable,
        }
    }
    #[cfg(not(feature = "ladybug"))]
    {
        let _ = config;
        ComponentStatus::Unavailable
    }
}

async fn check_llm(config: Config) -> ComponentStatus {
    let Ok(provider) = crate::llm::Ollama::new(
        config.llm().base_url().clone(),
        config.llm().health_timeout(),
    ) else {
        return ComponentStatus::Unavailable;
    };
    match provider.verify_endpoint().await {
        Ok(()) => ComponentStatus::Available,
        Err(_) => ComponentStatus::Unavailable,
    }
}

const fn overall_status(
    control_store: ComponentStatus,
    knowledge_store: ComponentStatus,
    llm: ComponentStatus,
) -> ServiceStatus {
    if matches!(control_store, ComponentStatus::Unavailable)
        || matches!(knowledge_store, ComponentStatus::Unavailable)
    {
        ServiceStatus::Offline
    } else if matches!(llm, ComponentStatus::Unavailable) {
        ServiceStatus::Degraded
    } else {
        ServiceStatus::Healthy
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MetricsResponse {
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

impl From<ops::MetricsReport> for MetricsResponse {
    fn from(report: ops::MetricsReport) -> Self {
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

async fn metrics(State(state): State<Arc<AppState>>) -> Result<Json<MetricsResponse>, ApiError> {
    let config = state.config.clone();
    let report = tokio::task::spawn_blocking(move || ops::metrics_read_only(&config))
        .await
        .map_err(|error| ApiError::internal(format!("metrics task failed: {error}")))?
        .map_err(|error| ApiError::service_unavailable(&error))?;
    Ok(Json(report.into()))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct QueryRequest {
    query: String,
    top_k: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryResponse {
    answer: Option<String>,
    citations: Vec<String>,
    chunks: Vec<ScoredChunkResponse>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScoredChunkResponse {
    chunk_id: String,
    score: f32,
    text: String,
}

impl From<ScoredChunk> for ScoredChunkResponse {
    fn from(chunk: ScoredChunk) -> Self {
        Self {
            chunk_id: chunk.chunk_id,
            score: chunk.score,
            text: chunk.text,
        }
    }
}

async fn query(
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
            .block_on(pipeline::answer(config, &query_text, top_k))
            .map_err(|error| ApiError::query(&error))
    })
    .await
    .map_err(|error| ApiError::internal(format!("query task failed: {error}")))??;
    Ok(Json(QueryResponse {
        answer: response.answer,
        citations: response.citations,
        chunks: response.chunks.into_iter().map(Into::into).collect(),
    }))
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn internal(message: String) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message,
        }
    }

    fn query(error: &pipeline::QueryError) -> Self {
        let status = match &error {
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

    fn service_unavailable(error: &ops::OpsError) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: error.to_string(),
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

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("ohara: failed to install Ctrl-C handler: {error}");
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::{ComponentStatus, ServiceStatus, overall_status, router};
    use crate::config::Config;

    fn test_config() -> (tempfile::TempDir, Config) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config_path = directory.path().join("ohara.toml");
        std::fs::write(&config_path, format!("data_dir = {:?}\n", directory.path()))
            .expect("config file");
        let config = Config::load(Some(&config_path)).expect("default config");
        (directory, config)
    }

    #[test]
    fn overall_health_degrades_when_llm_is_unavailable() {
        assert!(matches!(
            overall_status(
                ComponentStatus::Available,
                ComponentStatus::Available,
                ComponentStatus::Unavailable,
            ),
            ServiceStatus::Degraded
        ));
    }

    #[tokio::test]
    async fn metrics_route_returns_camel_case_snapshot() {
        let (_directory, config) = test_config();
        let response = router(config)
            .oneshot(
                Request::builder()
                    .uri("/api/metrics")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert!(json.get("capturedAt").is_some());
        assert!(json.get("jobsByStageStatus").is_some());
        assert!(json.get("llmUsage").is_some());
    }

    #[tokio::test]
    async fn query_route_rejects_empty_input_at_the_boundary() {
        let (_directory, config) = test_config();
        let response = router(config)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/query")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"query":""}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
