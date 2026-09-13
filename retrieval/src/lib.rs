//! Retrieval process.
//!
//! This package owns the frontend-facing HTTP interface and read-side query
//! composition. It reads catalog/index artifacts, queries Qdrant and `FalkorDB`,
//! and calls the configured local Ollama model for synthesis. It does not run
//! ingestion stages.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{Path as AxumPath, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use redis::AsyncCommands;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;

mod metrics;

const DEFAULT_TOP_K: usize = 5;
const MAX_TOP_K: usize = 20;
const SCRAPER_URL: &str = "http://127.0.0.1:3010";
const QDRANT_URL: &str = "http://127.0.0.1:6335";
const FALKORDB_URL: &str = "redis://127.0.0.1:6380";
const LLM_URL: &str = "http://127.0.0.1:11434";
const LLM_MODEL: &str = "phi4-mini:latest";
const EMBEDDING_DIMENSION: usize = 384;
const PROCESS_AUTH_ENV: &str = "OHARA_PROCESS_AUTH_TOKEN";

/// Errors raised while starting the retrieval process.
#[derive(Debug, thiserror::Error)]
pub enum RetrievalError {
    /// The process configuration is invalid.
    #[error("configuration: {0}")]
    Configuration(String),
    /// A local file operation failed.
    #[error("storage: {0}")]
    Storage(#[from] std::io::Error),
    /// A provider client could not be created.
    #[error("provider client: {0}")]
    Provider(String),
    /// The listener failed.
    #[error("listener: {0}")]
    Listener(std::io::Error),
}

#[derive(Clone)]
struct AppState {
    data_dir: PathBuf,
    client: Client,
    qdrant_url: String,
    falkordb_url: String,
    scraper_url: String,
    process_auth_token: Option<String>,
    llm_url: String,
    llm_model: String,
    embedder: Arc<Mutex<Option<EmbeddingBackend>>>,
    metrics: Arc<metrics::Metrics>,
}

enum EmbeddingBackend {
    FastEmbed(Box<TextEmbedding>),
    Deterministic,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TopicRequest {
    topic: String,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TopicResponse {
    topic: String,
    requested: usize,
    discovered: usize,
    enqueued: usize,
    duplicates: usize,
    documents: Vec<TopicDocument>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TopicDocument {
    document_id: String,
    job_id: Option<String>,
    source_url: String,
    status: String,
    title: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogDocument {
    id: String,
    source_url: String,
    title: Option<String>,
    status: String,
    chunk_count: usize,
    created_at: String,
    last_processed_at: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DocumentResponse {
    id: String,
    source_url: String,
    title: Option<String>,
    status: String,
    chunk_count: usize,
    created_at: String,
    last_processed_at: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryResponse {
    answer: Option<String>,
    availability: &'static str,
    citations: Vec<String>,
    chunks: Vec<QueryChunk>,
    grounding: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryChunk {
    chunk_id: String,
    score: f32,
    text: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueryRequest {
    query: String,
    top_k: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct QdrantSearchResponse {
    result: Vec<QdrantPoint>,
}

#[derive(Debug, Deserialize)]
struct QdrantPoint {
    score: f32,
    payload: Option<BTreeMap<String, serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
struct OllamaResponse {
    response: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DocumentQuery {
    cursor: Option<usize>,
    limit: Option<usize>,
    search: Option<String>,
    status: Option<String>,
}

/// Runs the frontend-facing retrieval HTTP process until interrupted.
///
/// # Errors
///
/// Returns an error when the artifact directory, provider clients, or HTTP
/// listener cannot be prepared.
pub async fn run(bind: SocketAddr) -> Result<(), RetrievalError> {
    let data_dir = data_dir();
    ensure_layout(&data_dir).await?;
    let client = Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|error| RetrievalError::Provider(error.to_string()))?;
    let metrics = Arc::new(metrics::Metrics::open(&data_dir, "retrieval").await?);
    let state = Arc::new(AppState {
        data_dir,
        client,
        qdrant_url: std::env::var("OHARA_QDRANT_URL").unwrap_or_else(|_| QDRANT_URL.into()),
        falkordb_url: std::env::var("OHARA_FALKORDB_URL").unwrap_or_else(|_| FALKORDB_URL.into()),
        scraper_url: std::env::var("OHARA_SCRAPER_URL").unwrap_or_else(|_| SCRAPER_URL.into()),
        process_auth_token: process_auth_token(),
        llm_url: std::env::var("OHARA_LLM_URL").unwrap_or_else(|_| LLM_URL.into()),
        llm_model: std::env::var("OHARA_LLM_MODEL").unwrap_or_else(|_| LLM_MODEL.into()),
        embedder: Arc::new(Mutex::new(None)),
        metrics,
    });
    let heartbeat_dir = state.data_dir.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            if let Err(error) = atomic_write(
                &heartbeat_dir.join("state/retrieval.heartbeat"),
                timestamp().as_bytes(),
            )
            .await
            {
                eprintln!("ohara-retrieval: heartbeat failed: {error}");
            }
        }
    });
    atomic_write(
        &state.data_dir.join("state/retrieval.heartbeat"),
        timestamp().as_bytes(),
    )
    .await?;

    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/overview", get(overview))
        .route("/api/documents", get(documents))
        .route("/api/metrics", get(metrics_endpoint))
        .route("/api/query", post(query))
        .route("/api/topics/scrape", post(scrape_topic))
        .route("/api/entities/reviews", get(entity_reviews))
        .route(
            "/api/entities/reviews/{review_id}/preview",
            get(entity_preview),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            record_metrics,
        ))
        .with_state(state);
    let listener = TcpListener::bind(bind).await?;
    eprintln!("ohara-retrieval: listening on http://{bind}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(RetrievalError::Listener)
}

async fn health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let artifact_store = tokio::fs::try_exists(&state.data_dir)
        .await
        .unwrap_or(false);
    let qdrant = qdrant_ready(&state).await;
    let falkordb = falkordb_ready(&state).await;
    let ollama = llm_ready(&state).await;
    let embedding_model = embedding_model_available(&state.data_dir);
    let scraper = process_ready(&state.data_dir, "scraper").await;
    let cleaning = process_ready(&state.data_dir, "cleaning").await;
    let indexer = process_ready(&state.data_dir, "indexer").await;
    let graph = process_ready(&state.data_dir, "graph").await;
    let mut diagnostics = Vec::new();
    if !artifact_store {
        diagnostics.push(diagnostic(
            "artifactStore",
            "artifact directory is unavailable",
            "Create the configured data directory and restart the retrieval process.",
        ));
    }
    if !qdrant {
        diagnostics.push(diagnostic(
            "qdrant",
            "Qdrant is unavailable",
            "Start Qdrant with docker compose.",
        ));
    }
    if !falkordb {
        diagnostics.push(diagnostic(
            "falkordb",
            "FalkorDB is unavailable",
            "Start FalkorDB with docker compose.",
        ));
    }
    if !embedding_model {
        diagnostics.push(diagnostic(
            "embeddingModel",
            "the local embedding model is not cached",
            "Start the indexer once with network access to download the embedding model.",
        ));
    }
    if !ollama {
        diagnostics.push(diagnostic(
            "ollama",
            "the configured Ollama model is unavailable",
            "Start Ollama and pull the configured model.",
        ));
    }
    for (name, ready) in [
        ("scraper", scraper),
        ("cleaning", cleaning),
        ("indexer", indexer),
        ("graph", graph),
    ] {
        if !ready {
            diagnostics.push(diagnostic(
                name,
                "process heartbeat is missing or stale",
                "Start or restart this process.",
            ));
        }
    }
    let status = if !artifact_store {
        "offline"
    } else if !qdrant
        || !falkordb
        || !embedding_model
        || !ollama
        || !scraper
        || !cleaning
        || !indexer
        || !graph
    {
        "degraded"
    } else {
        "healthy"
    };
    Json(serde_json::json!({
        "status": status,
        "processes": {
            "scraper": if scraper { "available" } else { "unavailable" },
            "cleaning": if cleaning { "available" } else { "unavailable" },
            "indexer": if indexer { "available" } else { "unavailable" },
            "graph": if graph { "available" } else { "unavailable" },
            "retrieval": "available"
        },
        "providers": {
            "artifactStore": if artifact_store { "available" } else { "unavailable" },
            "qdrant": if qdrant { "available" } else { "unavailable" },
            "falkordb": if falkordb { "available" } else { "unavailable" },
            "embeddingModel": if embedding_model { "available" } else { "unavailable" },
            "ollama": if ollama { "available" } else { "unavailable" }
        },
        "diagnostics": diagnostics
    }))
}

fn diagnostic(
    component: &'static str,
    message: &'static str,
    action: &'static str,
) -> serde_json::Value {
    serde_json::json!({ "component": component, "message": message, "action": action })
}

async fn overview(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, RetrievalHttpError> {
    let catalog = read_catalog(&state.data_dir).await?;
    let mut counts = BTreeMap::<String, usize>::new();
    for document in &catalog {
        *counts.entry(document.status.clone()).or_default() += 1;
    }
    let queue = queue_items(&state.data_dir, &catalog).await?;
    Ok(Json(
        serde_json::json!({ "documentsByStatus": counts, "queue": queue }),
    ))
}

async fn documents(
    State(state): State<Arc<AppState>>,
    Query(query): Query<DocumentQuery>,
) -> Result<Json<serde_json::Value>, RetrievalHttpError> {
    let limit = query.limit.unwrap_or(25).clamp(1, 100);
    let cursor = query.cursor.unwrap_or(0);
    let search = query.search.unwrap_or_default().to_ascii_lowercase();
    let status = query.status.map(|value| value.to_ascii_uppercase());
    let mut catalog = read_catalog(&state.data_dir).await?;
    catalog.retain(|document| {
        let matches_status = status
            .as_ref()
            .is_none_or(|wanted| &document.status == wanted);
        let matches_search = search.is_empty()
            || document.source_url.to_ascii_lowercase().contains(&search)
            || document
                .title
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase()
                .contains(&search);
        matches_status && matches_search
    });
    let items: Vec<DocumentResponse> = catalog
        .into_iter()
        .skip(cursor)
        .take(limit)
        .map(DocumentResponse::from)
        .collect();
    let next_cursor = (items.len() == limit).then_some(cursor + items.len());
    Ok(Json(
        serde_json::json!({ "items": items, "nextCursor": next_cursor }),
    ))
}

impl From<CatalogDocument> for DocumentResponse {
    fn from(document: CatalogDocument) -> Self {
        Self {
            id: document.id,
            source_url: document.source_url,
            title: document.title,
            status: document.status,
            chunk_count: document.chunk_count,
            created_at: document.created_at,
            last_processed_at: document.last_processed_at,
            error: document.error,
        }
    }
}

async fn metrics_endpoint(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, RetrievalHttpError> {
    let catalog = read_catalog(&state.data_dir).await?;
    let mut documents_by_status = BTreeMap::<String, usize>::new();
    for document in &catalog {
        *documents_by_status
            .entry(document.status.clone())
            .or_default() += 1;
    }
    let (raw_files, raw_bytes) = raw_usage(&state.data_dir).await?;
    let mut stages = BTreeMap::new();
    for process in ["scraper", "cleaning", "indexer", "graph"] {
        stages.insert(
            process,
            metrics::read_or_default(&state.data_dir, process).await?,
        );
    }
    stages.insert("retrieval", state.metrics.snapshot().await);
    Ok(Json(serde_json::json!({
        "capturedAt": timestamp(),
        "documentsByStatus": documents_by_status,
        "dueForRecrawl": 0,
        "eventsByOutcome": {},
        "eventsByStage": {},
        "jobsByStageStatus": {},
        "llmUsage": {"calls": 0, "successfulCalls": 0, "failedCalls": 0, "promptTokens": 0, "completionTokens": 0, "estimatedCostMicros": 0},
        "pendingErReviews": 0,
        "rawBytes": raw_bytes,
        "rawFiles": raw_files,
        "rawMaxAgeDays": serde_json::Value::Null,
        "rawMaxBytes": serde_json::Value::Null,
        "stages": stages
    })))
}

async fn record_metrics(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let started = Instant::now();
    let response = next.run(request).await;
    let succeeded = response.status().is_success();
    if let Err(error) = state
        .metrics
        .record(
            1,
            u64::from(succeeded),
            u64::from(!succeeded),
            started.elapsed(),
        )
        .await
    {
        eprintln!("ohara-retrieval: metrics write failed: {error}");
    }
    response
}

async fn scrape_topic(
    State(state): State<Arc<AppState>>,
    Json(request): Json<TopicRequest>,
) -> Result<Json<TopicResponse>, RetrievalHttpError> {
    let mut request_builder = state
        .client
        .post(format!(
            "{}/scrape",
            state.scraper_url.trim_end_matches('/')
        ))
        .json(&request);
    if let Some(token) = state.process_auth_token.as_deref() {
        request_builder = request_builder.bearer_auth(token);
    }
    let response = request_builder
        .send()
        .await
        .map_err(|error| RetrievalHttpError::BadGateway(error.to_string()))?;
    let status = response.status();
    let payload = response
        .json::<TopicResponse>()
        .await
        .map_err(|error| RetrievalHttpError::BadGateway(error.to_string()))?;
    if !status.is_success() {
        return Err(RetrievalHttpError::BadGateway(format!(
            "scraper returned {status}"
        )));
    }
    Ok(Json(payload))
}

async fn query(
    State(state): State<Arc<AppState>>,
    Json(request): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, RetrievalHttpError> {
    let question = request.query.trim();
    if question.is_empty() {
        return Err(RetrievalHttpError::BadRequest(
            "query must not be empty".into(),
        ));
    }
    let top_k = request.top_k.unwrap_or(DEFAULT_TOP_K).clamp(1, MAX_TOP_K);
    let vector = embed(&state, question)?;
    let points = search_qdrant(&state, &vector, top_k).await?;
    let chunks: Vec<QueryChunk> = points.iter().filter_map(point_to_chunk).collect();
    if chunks.is_empty() {
        return Ok(Json(QueryResponse {
            answer: None,
            availability: "available",
            citations: Vec::new(),
            chunks,
            grounding: "ungrounded",
        }));
    }
    let prompt = render_prompt(question, &chunks);
    let answer = call_llm(&state, &prompt).await;
    let (answer, availability, grounding, citations) = match answer {
        Ok(Some(value)) if !value.trim().is_empty() => (
            Some(value),
            "available",
            "grounded",
            chunks.iter().map(|chunk| chunk.chunk_id.clone()).collect(),
        ),
        Ok(_) => (None, "available", "ungrounded", Vec::new()),
        Err(_) => (None, "unavailable", "ungrounded", Vec::new()),
    };
    Ok(Json(QueryResponse {
        answer,
        availability,
        citations,
        chunks,
        grounding,
    }))
}

async fn entity_reviews() -> Json<Vec<serde_json::Value>> {
    Json(Vec::new())
}

async fn entity_preview(
    AxumPath(_review_id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, RetrievalHttpError> {
    Err(RetrievalHttpError::NotFound)
}

fn embed(state: &AppState, text: &str) -> Result<Vec<f32>, RetrievalHttpError> {
    let mut guard = state
        .embedder
        .lock()
        .map_err(|_| RetrievalHttpError::Internal("embedder mutex poisoned".into()))?;
    if guard.is_none() {
        *guard = Some(
            if std::env::var("OHARA_EMBEDDING_MODE").as_deref() == Ok("deterministic") {
                EmbeddingBackend::Deterministic
            } else {
                let model = TextEmbedding::try_new(
                    InitOptions::new(EmbeddingModel::BGESmallENV15)
                        .with_cache_dir(state.data_dir.join("models"))
                        .with_max_length(512)
                        .with_show_download_progress(false),
                )
                .map_err(|error| {
                    RetrievalHttpError::ServiceUnavailable(format!(
                        "embedding model unavailable: {error}"
                    ))
                })?;
                EmbeddingBackend::FastEmbed(Box::new(model))
            },
        );
    }
    let backend = guard
        .as_mut()
        .ok_or_else(|| RetrievalHttpError::Internal("embedder was not initialized".into()))?;
    match backend {
        EmbeddingBackend::Deterministic => Ok(deterministic_embedding(text)),
        EmbeddingBackend::FastEmbed(model) => model
            .embed(vec![text], None)
            .map_err(|error| {
                RetrievalHttpError::ServiceUnavailable(format!("embedding failed: {error}"))
            })?
            .into_iter()
            .next()
            .ok_or_else(|| RetrievalHttpError::Internal("embedder returned no vector".into())),
    }
}

fn deterministic_embedding(text: &str) -> Vec<f32> {
    (0..EMBEDDING_DIMENSION)
        .map(|index| {
            let mut digest = Sha256::new();
            digest.update(text.as_bytes());
            digest.update(index.to_le_bytes());
            let bytes = digest.finalize();
            let value = u16::from_le_bytes([bytes[0], bytes[1]]);
            (f32::from(value) / f32::from(u16::MAX)) * 2.0 - 1.0
        })
        .collect()
}

async fn search_qdrant(
    state: &AppState,
    vector: &[f32],
    limit: usize,
) -> Result<Vec<QdrantPoint>, RetrievalHttpError> {
    let response = state
        .client
        .post(format!(
            "{}/collections/ohara_chunks/points/search",
            state.qdrant_url.trim_end_matches('/')
        ))
        .json(&serde_json::json!({"vector": vector, "limit": limit, "with_payload": true}))
        .send()
        .await
        .map_err(|error| {
            RetrievalHttpError::ServiceUnavailable(format!("qdrant unavailable: {error}"))
        })?;
    if !response.status().is_success() {
        return Err(RetrievalHttpError::ServiceUnavailable(format!(
            "qdrant returned {}",
            response.status()
        )));
    }
    response
        .json::<QdrantSearchResponse>()
        .await
        .map(|result| result.result)
        .map_err(|error| {
            RetrievalHttpError::BadGateway(format!("invalid qdrant response: {error}"))
        })
}

fn point_to_chunk(point: &QdrantPoint) -> Option<QueryChunk> {
    let payload = point.payload.as_ref()?;
    Some(QueryChunk {
        chunk_id: payload.get("chunkId")?.as_str()?.to_string(),
        score: point.score,
        text: payload.get("text")?.as_str()?.to_string(),
    })
}

fn render_prompt(question: &str, chunks: &[QueryChunk]) -> String {
    let evidence = chunks
        .iter()
        .map(|chunk| format!("[{}]\n{}", chunk.chunk_id, chunk.text))
        .collect::<Vec<_>>()
        .join("\n\n");
    format!(
        "Answer the question using only the evidence below. If the evidence does not support an answer, say that it is not known. Do not invent facts.\n\nQuestion: {question}\n\nEvidence:\n{evidence}\n\nAnswer concisely:"
    )
}

async fn call_llm(state: &AppState, prompt: &str) -> Result<Option<String>, RetrievalError> {
    let response = state
        .client
        .post(format!("{}/api/generate", state.llm_url.trim_end_matches('/')))
        .json(&serde_json::json!({"model": state.llm_model, "prompt": prompt, "stream": false, "options": {"temperature": 0}}))
        .send()
        .await
        .map_err(|error| RetrievalError::Provider(error.to_string()))?;
    if !response.status().is_success() {
        return Err(RetrievalError::Provider(format!(
            "ollama returned {}",
            response.status()
        )));
    }
    let payload = response
        .json::<OllamaResponse>()
        .await
        .map_err(|error| RetrievalError::Provider(error.to_string()))?;
    Ok(payload.response)
}

async fn read_catalog(data_dir: &Path) -> Result<Vec<CatalogDocument>, RetrievalHttpError> {
    let mut entries = tokio::fs::read_dir(data_dir.join("catalog")).await?;
    let mut documents = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_file()
            || entry.path().extension().is_none_or(|ext| ext != "json")
        {
            continue;
        }
        if let Ok(bytes) = tokio::fs::read(entry.path()).await
            && let Ok(document) = serde_json::from_slice::<CatalogDocument>(&bytes)
        {
            documents.push(document);
        }
    }
    documents.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(documents)
}

async fn queue_items(
    data_dir: &Path,
    catalog: &[CatalogDocument],
) -> Result<Vec<serde_json::Value>, RetrievalHttpError> {
    let mut output = Vec::new();
    for (directory, stage) in [
        ("cleaning", "SCRAPE"),
        ("indexer", "CLEAN"),
        ("graph", "EXTRACT"),
    ] {
        let mut entries = tokio::fs::read_dir(data_dir.join("inbox").join(directory)).await?;
        while let Some(entry) = entries.next_entry().await? {
            let entry_path = entry.path();
            let Some(id) = entry_path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            let document = catalog.iter().find(|item| item.id == id);
            output.push(serde_json::json!({
                "documentId": id,
                "jobId": id,
                "documentStatus": document.map_or("NEW", |item| item.status.as_str()),
                "error": document.and_then(|item| item.error.clone()),
                "jobStatus": "PENDING",
                "sourceUrl": document.map_or("", |item| item.source_url.as_str()),
                "stage": stage,
                "title": document.and_then(|item| item.title.clone()),
                "updatedAt": timestamp()
            }));
        }
    }
    Ok(output)
}

async fn raw_usage(data_dir: &Path) -> Result<(usize, u64), RetrievalHttpError> {
    let mut entries = tokio::fs::read_dir(data_dir.join("raw")).await?;
    let mut files = 0;
    let mut bytes: u64 = 0;
    while let Some(entry) = entries.next_entry().await? {
        if entry.path().extension().is_some_and(|ext| ext == "html") {
            files += 1;
            bytes = bytes.saturating_add(entry.metadata().await?.len());
        }
    }
    Ok((files, bytes))
}

async fn qdrant_ready(state: &AppState) -> bool {
    state
        .client
        .get(format!(
            "{}/collections",
            state.qdrant_url.trim_end_matches('/')
        ))
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}

async fn falkordb_ready(state: &AppState) -> bool {
    let Ok(client) = redis::Client::open(state.falkordb_url.clone()) else {
        return false;
    };
    let Ok(mut connection) = client.get_multiplexed_async_connection().await else {
        return false;
    };
    connection.ping::<String>().await.is_ok()
}

async fn llm_ready(state: &AppState) -> bool {
    state
        .client
        .get(format!("{}/api/tags", state.llm_url.trim_end_matches('/')))
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}

fn embedding_model_available(data_dir: &Path) -> bool {
    if std::env::var("OHARA_EMBEDDING_MODE").as_deref() == Ok("deterministic") {
        return true;
    }
    let root = data_dir.join("models/models--Xenova--bge-small-en-v1.5");
    root.join("refs/main").is_file() && root.join("snapshots").is_dir()
}

async fn process_ready(data_dir: &Path, name: &str) -> bool {
    let Ok(metadata) = tokio::fs::metadata(data_dir.join(format!("state/{name}.heartbeat"))).await
    else {
        return false;
    };
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age < Duration::from_secs(15))
}

#[derive(Debug, thiserror::Error)]
enum RetrievalHttpError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("bad gateway: {0}")]
    BadGateway(String),
    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),
    #[error("not found")]
    NotFound,
    #[error("internal error: {0}")]
    Internal(String),
    #[error("storage: {0}")]
    Storage(#[from] std::io::Error),
}

impl IntoResponse for RetrievalHttpError {
    fn into_response(self) -> axum::response::Response {
        let status = match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::ServiceUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::BadGateway(_) => StatusCode::BAD_GATEWAY,
            Self::Internal(_) | Self::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (
            status,
            Json(serde_json::json!({ "error": self.to_string() })),
        )
            .into_response()
    }
}

impl From<RetrievalError> for RetrievalHttpError {
    fn from(error: RetrievalError) -> Self {
        Self::BadGateway(error.to_string())
    }
}

async fn ensure_layout(data_dir: &Path) -> Result<(), std::io::Error> {
    for relative in [
        "raw",
        "clean",
        "indexed",
        "catalog",
        "inbox/cleaning",
        "inbox/indexer",
        "inbox/graph",
        "state",
        "models",
    ] {
        tokio::fs::create_dir_all(data_dir.join(relative)).await?;
    }
    Ok(())
}
async fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    tokio::fs::write(&temporary, bytes).await?;
    tokio::fs::rename(temporary, path).await
}
fn data_dir() -> PathBuf {
    std::env::var_os("OHARA_DATA_DIR").map_or_else(|| PathBuf::from("data"), PathBuf::from)
}

fn process_auth_token() -> Option<String> {
    std::env::var(PROCESS_AUTH_ENV)
        .ok()
        .filter(|token| !token.trim().is_empty())
}
fn timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or_else(|_| "0".into(), |value| value.as_secs().to_string())
}
async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("ohara-retrieval: shutdown signal failed: {error}");
    }
}
