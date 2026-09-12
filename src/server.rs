//! Local HTTP transport for the frontend operator surface.
//!
//! The server is an optional process boundary. It translates JSON requests into
//! calls to the existing pipeline and operator facades; datastore access stays
//! behind those facades.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use crate::config::Config;
use crate::control;
use crate::ops;
use crate::pipeline::{self, ScoredChunk};

const DEFAULT_DOCUMENT_PAGE_SIZE: usize = 25;
const MAX_DOCUMENT_PAGE_SIZE: usize = crate::control::MAX_DOCUMENT_PAGE_SIZE;
const OVERVIEW_QUEUE_LIMIT: usize = 10;

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
        .route("/api/overview", get(overview))
        .route("/api/documents", get(documents))
        .route("/api/entities/reviews", get(entity_reviews))
        .route(
            "/api/entities/reviews/{review_id}/preview",
            get(entity_review_preview),
        )
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

#[derive(Debug)]
struct HealthCheck {
    status: ComponentStatus,
    diagnostic: Option<ReadinessDiagnostic>,
}

impl HealthCheck {
    fn available() -> Self {
        Self {
            status: ComponentStatus::Available,
            diagnostic: None,
        }
    }

    fn unavailable(
        component: &'static str,
        message: impl Into<String>,
        action: impl Into<String>,
    ) -> Self {
        Self {
            status: ComponentStatus::Unavailable,
            diagnostic: Some(ReadinessDiagnostic {
                component,
                message: message.into(),
                action: action.into(),
            }),
        }
    }
}

async fn health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let control_store = check_control_store(state.config.clone()).await;
    let knowledge_store = check_knowledge_store(state.config.clone()).await;
    let embedder = check_embedder(state.config.clone()).await;
    let llm = check_llm(state.config.clone()).await;
    let status = overall_status(
        control_store.status,
        knowledge_store.status,
        embedder.status,
        llm.status,
    );
    let diagnostics = [
        control_store.diagnostic,
        knowledge_store.diagnostic,
        embedder.diagnostic,
        llm.diagnostic,
    ]
    .into_iter()
    .flatten()
    .collect();
    Json(HealthResponse {
        status,
        control_store: control_store.status,
        knowledge_store: knowledge_store.status,
        embedder: embedder.status,
        llm: llm.status,
        reranker: "identity",
        diagnostics,
    })
}

async fn check_control_store(config: Config) -> HealthCheck {
    let result =
        tokio::task::spawn_blocking(move || crate::control::connect(config.db_path()).map(|_| ()))
            .await;
    match result {
        Ok(Ok(())) => HealthCheck::available(),
        Ok(Err(error)) => HealthCheck::unavailable(
            "controlStore",
            format!("control store is unavailable: {error}"),
            "Check the configured database path and permissions.",
        ),
        Err(error) => HealthCheck::unavailable(
            "controlStore",
            format!("control store health check failed: {error}"),
            "Restart the local API and check the process logs.",
        ),
    }
}

async fn check_knowledge_store(config: Config) -> HealthCheck {
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
            Ok(Ok(())) => HealthCheck::available(),
            Ok(Err(error)) => HealthCheck::unavailable(
                "knowledgeStore",
                format!("knowledge store is unavailable: {error}"),
                "Repair or rebuild the local knowledge index, then restart Ohara.",
            ),
            Err(error) => HealthCheck::unavailable(
                "knowledgeStore",
                format!("knowledge store health check failed: {error}"),
                "Restart the local API and check the knowledge-store logs.",
            ),
        }
    }
    #[cfg(not(feature = "ladybug"))]
    {
        let _ = config;
        HealthCheck::unavailable(
            "knowledgeStore",
            "Ohara was built without the `ladybug` feature.",
            "Use the default feature set or inject a KnowledgeStore.",
        )
    }
}

async fn check_embedder(config: Config) -> HealthCheck {
    let result =
        tokio::task::spawn_blocking(move || crate::pipeline::check_embedder_readiness(&config))
            .await;
    match result {
        Ok(Ok(())) => HealthCheck::available(),
        Ok(Err(error)) => HealthCheck::unavailable(
            "embedder",
            error.to_string(),
            "Run the worker once while online to download the pinned embedding model, then retry.",
        ),
        Err(error) => HealthCheck::unavailable(
            "embedder",
            format!("embedder health check failed: {error}"),
            "Restart the local API and check the model-cache permissions.",
        ),
    }
}

async fn check_llm(config: Config) -> HealthCheck {
    let Ok(provider) = crate::llm::Ollama::new(
        config.llm().base_url().clone(),
        config.llm().health_timeout(),
    ) else {
        return HealthCheck::unavailable(
            "llm",
            "the configured local language-model client could not be created",
            "Check the configured LLM URL and restart the local API.",
        );
    };
    match provider.verify_endpoint().await {
        Ok(()) => HealthCheck::available(),
        Err(error) => HealthCheck::unavailable(
            "llm",
            format!("local language-model endpoint is unavailable: {error}"),
            "Start the configured local provider and retry.",
        ),
    }
}

const fn overall_status(
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OverviewResponse {
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

impl From<control::QueueItem> for QueueResponse {
    fn from(item: control::QueueItem) -> Self {
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

async fn overview(State(state): State<Arc<AppState>>) -> Result<Json<OverviewResponse>, ApiError> {
    let config = state.config.clone();
    let snapshot = tokio::task::spawn_blocking(move || {
        let db = control::connect(config.db_path())?;
        control::overview(&db, OVERVIEW_QUEUE_LIMIT)
    })
    .await
    .map_err(|error| ApiError::internal(format!("overview task failed: {error}")))?
    .map_err(|error| ApiError::control(&error))?;
    Ok(Json(OverviewResponse {
        documents_by_status: snapshot.documents_by_status,
        queue: snapshot.queue.into_iter().map(Into::into).collect(),
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DocumentRequest {
    limit: Option<usize>,
    cursor: Option<String>,
    status: Option<String>,
    search: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DocumentPageResponse {
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

impl From<control::DocumentListItem> for DocumentResponse {
    fn from(item: control::DocumentListItem) -> Self {
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

fn document_query(request: DocumentRequest) -> Result<control::DocumentListQuery, ApiError> {
    let limit = request.limit.unwrap_or(DEFAULT_DOCUMENT_PAGE_SIZE);
    if !(1..=MAX_DOCUMENT_PAGE_SIZE).contains(&limit) {
        return Err(ApiError::bad_request(format!(
            "document limit must be between 1 and {MAX_DOCUMENT_PAGE_SIZE}"
        )));
    }
    let status = request.status.map(|value| {
        value
            .parse::<control::DocStatus>()
            .map_err(|()| ApiError::bad_request(format!("unknown document status {value:?}")))
    });
    let status = status.transpose()?;
    let cursor = request.cursor.filter(|value| !value.trim().is_empty());
    let search = request.search.and_then(|value| {
        let value = value.trim().to_string();
        (!value.is_empty()).then_some(value)
    });
    Ok(control::DocumentListQuery {
        limit,
        cursor,
        status,
        search,
    })
}

async fn documents(
    State(state): State<Arc<AppState>>,
    Query(request): Query<DocumentRequest>,
) -> Result<Json<DocumentPageResponse>, ApiError> {
    let query = document_query(request)?;
    let config = state.config.clone();
    let page = tokio::task::spawn_blocking(move || {
        let db = control::connect(config.db_path())?;
        control::list_documents(&db, &query)
    })
    .await
    .map_err(|error| ApiError::internal(format!("documents task failed: {error}")))?
    .map_err(|error| ApiError::control(&error))?;
    Ok(Json(DocumentPageResponse {
        items: page.items.into_iter().map(Into::into).collect(),
        next_cursor: page.next_cursor,
    }))
}

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
struct EntityReviewResponse {
    id: String,
    candidate_a: EntitySummaryResponse,
    candidate_b: EntitySummaryResponse,
    score: Option<f64>,
}

impl From<control::EntityReviewItem> for EntityReviewResponse {
    fn from(item: control::EntityReviewItem) -> Self {
        Self {
            id: item.review_id.to_string(),
            candidate_a: item.entity_a.into(),
            candidate_b: item.entity_b.into(),
            score: item.score,
        }
    }
}

impl From<control::EntitySummary> for EntitySummaryResponse {
    fn from(entity: control::EntitySummary) -> Self {
        Self {
            aliases: entity.alias_count,
            id: entity.entity_id,
            name: entity.canonical_name,
            entity_type: entity.entity_type,
        }
    }
}

async fn entity_reviews(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<EntityReviewResponse>>, ApiError> {
    let config = state.config.clone();
    let reviews = tokio::task::spawn_blocking(move || {
        let db = control::connect(config.db_path())?;
        control::entity_review_items(&db)
    })
    .await
    .map_err(|error| ApiError::internal(format!("entity reviews task failed: {error}")))?
    .map_err(|error| ApiError::control(&error))?;
    Ok(Json(reviews.into_iter().map(Into::into).collect()))
}

async fn entity_review_preview(
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
        let db = control::connect(config.db_path())?;
        control::entity_review_items(&db).map(|reviews| {
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
    availability: pipeline::QueryAvailability,
    grounding: pipeline::QueryGrounding,
    reranker: &'static str,
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
        availability: response.availability,
        grounding: response.grounding,
        reranker: "identity",
    }))
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: String) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message,
        }
    }

    fn not_found(message: String) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message,
        }
    }

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

    fn control(error: &control::DbError) -> Self {
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
                ComponentStatus::Available,
                ComponentStatus::Unavailable,
            ),
            ServiceStatus::Degraded
        ));
    }

    #[tokio::test]
    async fn metrics_route_returns_camel_case_snapshot() {
        let (_directory, config) = test_config();
        let response = router(config.clone())
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
        assert!(json["llmUsage"].get("successfulCalls").is_some());
        assert!(json["llmUsage"].get("successful_calls").is_none());
    }

    #[tokio::test]
    async fn health_route_reports_actionable_missing_model_diagnostics() {
        let (_directory, config) = test_config();
        let response = router(config)
            .oneshot(
                Request::builder()
                    .uri("/api/health")
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
        assert_eq!(json["embedder"], "unavailable");
        assert!(json["diagnostics"].as_array().is_some_and(|diagnostics| {
            diagnostics.iter().any(|diagnostic| {
                diagnostic["component"] == "embedder"
                    && diagnostic["message"]
                        .as_str()
                        .is_some_and(|message| message.contains("embedding model"))
                    && diagnostic["action"]
                        .as_str()
                        .is_some_and(|action| action.contains("download"))
            })
        }));
    }

    #[tokio::test]
    async fn health_route_reports_invalid_knowledge_artifact() {
        let (directory, config) = test_config();
        std::fs::write(directory.path().join("ladybug"), "not a directory")
            .expect("invalid knowledge artifact");
        let response = router(config)
            .oneshot(
                Request::builder()
                    .uri("/api/health")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let body = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["knowledgeStore"], "unavailable");
        assert!(json["diagnostics"].as_array().is_some_and(|diagnostics| {
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic["component"] == "knowledgeStore")
        }));
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

    #[tokio::test]
    async fn overview_route_returns_document_counts_and_queue_projection() {
        let (_directory, config) = test_config();
        let db = crate::control::connect(config.db_path()).expect("control store");
        db.raw()
            .execute(
                "INSERT INTO documents
                    (doc_id, source_url, source_url_normalized, raw_file_path, status, pipeline_version)
                 VALUES ('doc-1', 'https://one.test', 'https://one.test', 'raw/one', 'NEW', 'test')",
                [],
            )
            .expect("document");
        db.raw()
            .execute(
                "INSERT INTO jobs (job_id, doc_id, stage, status)
                 VALUES ('job-1', 'doc-1', 'SCRAPE', 'PENDING')",
                [],
            )
            .expect("job");
        drop(db);

        let response = router(config)
            .oneshot(
                Request::builder()
                    .uri("/api/overview")
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
        assert_eq!(json["documentsByStatus"]["NEW"], 1);
        assert_eq!(json["queue"][0]["jobStatus"], "PENDING");
        assert_eq!(json["queue"][0]["documentId"], "doc-1");
    }

    #[tokio::test]
    async fn documents_route_preserves_status_and_cursor_contract() {
        let (_directory, config) = test_config();
        let db = crate::control::connect(config.db_path()).expect("control store");
        for (id, status) in [("doc-b", "FAILED_QUALITY"), ("doc-a", "INDEXED")] {
            db.raw()
                .execute(
                    "INSERT INTO documents
                        (doc_id, source_url, source_url_normalized, raw_file_path, status, pipeline_version)
                     VALUES (?1, ?2, ?2, ?3, ?4, 'test')",
                    rusqlite::params![
                        id,
                        format!("https://{id}.test"),
                        format!("raw/{id}"),
                        status
                    ],
                )
                .expect("document");
        }
        drop(db);

        let response = router(config.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/documents?limit=1")
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
        assert_eq!(json["items"][0]["status"], "FAILED_QUALITY");
        assert_eq!(json["items"][0]["id"], "doc-b");
        assert_eq!(json["nextCursor"], "doc-b");

        let response = router(config)
            .oneshot(
                Request::builder()
                    .uri("/api/documents?limit=1&cursor=doc-b")
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
        assert_eq!(json["items"][0]["status"], "INDEXED");
        assert!(json["nextCursor"].is_null());
    }

    #[tokio::test]
    async fn entity_review_routes_return_hydrated_candidates() {
        let (_directory, config) = test_config();
        let db = crate::control::connect(config.db_path()).expect("control store");
        for (id, name) in [("entity-a", "Ohara"), ("entity-b", "O'Hara")] {
            db.raw()
                .execute(
                    "INSERT INTO entities (entity_id, canonical_name, entity_type)
                     VALUES (?1, ?2, 'PRODUCT')",
                    rusqlite::params![id, name],
                )
                .expect("entity");
        }
        db.raw()
            .execute(
                "INSERT INTO er_review (entity_a, entity_b, score, status)
                 VALUES ('entity-a', 'entity-b', 0.91, 'PENDING')",
                [],
            )
            .expect("review");
        drop(db);

        let response = router(config.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/entities/reviews")
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
        assert_eq!(json[0]["candidateA"]["name"], "Ohara");
        assert_eq!(json[0]["candidateB"]["type"], "PRODUCT");

        let response = router(config)
            .oneshot(
                Request::builder()
                    .uri("/api/entities/reviews/1/preview")
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
        assert_eq!(json["id"], "1");
        assert_eq!(json["score"], 0.91);
    }
}
