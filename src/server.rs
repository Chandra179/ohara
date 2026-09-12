//! Local HTTP transport for the frontend operator surface.
//!
//! The server is an optional process boundary. It translates JSON requests into
//! calls to the existing pipeline and operator facades; datastore access stays
//! behind those facades.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};
use tokio::net::TcpListener;

use crate::config::Config;

mod documents;
mod entities;
mod error;
mod health;
mod metrics;
mod overview;
mod query;

use self::health::health;

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
pub(super) struct AppState {
    pub(super) config: Config,
    pub(super) query_runtime: Arc<crate::runtime::QueryRuntime>,
}

fn router(config: Config) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/metrics", get(metrics::metrics))
        .route("/api/overview", get(overview::overview))
        .route("/api/documents", get(documents::documents))
        .route("/api/entities/reviews", get(entities::entity_reviews))
        .route(
            "/api/entities/reviews/{review_id}/preview",
            get(entities::entity_review_preview),
        )
        .route("/api/query", post(query::query))
        .with_state(Arc::new(AppState {
            config,
            query_runtime: Arc::new(crate::runtime::QueryRuntime::default()),
        }))
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

    use super::health::{ComponentStatus, ServiceStatus, overall_status};
    use super::router;
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
