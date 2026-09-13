//! Scraper process.
//!
//! This package owns topic discovery, URL validation, fetching, and raw
//! artifact publication. It communicates with the cleaning process through a
//! versioned JSON file handoff under the shared data directory.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;

mod config;
mod metrics;
mod providers;

const DEFAULT_LIMIT: usize = 5;
const MAX_LIMIT: usize = 10;
const MAX_TOPIC_CHARS: usize = 200;
pub(crate) const MAX_RSS_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_DOCUMENT_BYTES: usize = 10 * 1024 * 1024;
const ARTIFACT_VERSION: u8 = 1;
const PROCESS_AUTH_ENV: &str = "OHARA_PROCESS_AUTH_TOKEN";

/// Errors raised by the scraper process.
#[derive(Debug, thiserror::Error)]
pub enum ScraperError {
    /// The process configuration is invalid.
    #[error("configuration: {0}")]
    Configuration(String),
    /// A local artifact could not be read or written.
    #[error("storage: {0}")]
    Storage(#[from] std::io::Error),
    /// The search provider or target document could not be reached.
    #[error("network: {0}")]
    Network(String),
    /// The search response was not usable.
    #[error("invalid provider response: {0}")]
    InvalidResponse(String),
    /// The HTTP listener failed.
    #[error("listener: {0}")]
    Listener(std::io::Error),
}

#[derive(Clone)]
struct AppState {
    data_dir: PathBuf,
    client: Client,
    config: config::Config,
    process_auth_token: Option<String>,
    metrics: Arc<metrics::Metrics>,
}

/// A topic request accepted by the scraper process.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScrapeRequest {
    /// Human-entered topic text.
    pub topic: String,
    /// Maximum number of articles to discover and fetch.
    pub limit: Option<usize>,
}

/// A document returned to the frontend-facing process after topic ingestion.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrapeResponse {
    /// Normalized topic text.
    pub topic: String,
    /// Requested result count.
    pub requested: usize,
    /// Number of provider results accepted.
    pub discovered: usize,
    /// Number of new raw artifacts written.
    pub enqueued: usize,
    /// Number of documents already present in the artifact catalog.
    pub duplicates: usize,
    /// Per-result publication status.
    pub documents: Vec<QueuedDocument>,
}

/// The publication result for one discovered article.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedDocument {
    /// Stable content identity derived from the normalized URL.
    pub document_id: String,
    /// The stage handoff identity, present for newly queued work.
    pub job_id: Option<String>,
    /// Normalized source URL.
    pub source_url: String,
    /// `enqueued` or `duplicate`.
    pub status: &'static str,
    /// Provider title.
    pub title: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct TopicResult {
    pub(crate) title: String,
    pub(crate) url: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct RawArtifact {
    schema_version: u8,
    document_id: String,
    source_url: String,
    title: String,
    raw_path: String,
    fetched_at: String,
}

/// Runs the scraper HTTP process until it receives a shutdown signal.
///
/// # Errors
///
/// Returns an error when the HTTP client, artifact directories, or listener
/// cannot be prepared.
pub async fn run() -> Result<(), ScraperError> {
    let config = config::Config::load()?;
    let bind = config.bind;
    let data_dir = data_dir();
    ensure_layout(&data_dir).await?;
    heartbeat(&data_dir).await?;
    let heartbeat_dir = data_dir.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            if let Err(error) = heartbeat(&heartbeat_dir).await {
                eprintln!("ohara-scraper: heartbeat failed: {error}");
            }
        }
    });

    let client = Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent(format!("ohara-scraper/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| ScraperError::Configuration(error.to_string()))?;
    let metrics = Arc::new(metrics::Metrics::open(&data_dir, "scraper").await?);
    let state = Arc::new(AppState {
        data_dir,
        client,
        config,
        process_auth_token: process_auth_token(),
        metrics,
    });
    let app = Router::new()
        .route("/health", get(health))
        .route("/scrape", post(scrape))
        .with_state(state);
    let listener = TcpListener::bind(bind).await?;
    eprintln!("ohara-scraper: listening on http://{bind}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(ScraperError::Listener)
}

async fn health() -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn scrape(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<ScrapeRequest>,
) -> Result<Json<ScrapeResponse>, ScraperHttpError> {
    if let Some(expected) = state.process_auth_token.as_deref()
        && !authorized(&headers, expected)
    {
        return Err(ScraperHttpError::Unauthorized);
    }
    let started = Instant::now();
    let topic = request.topic.trim().to_string();
    if topic.is_empty() {
        return Err(ScraperHttpError::BadRequest(
            "topic must not be empty".into(),
        ));
    }
    if topic.chars().count() > MAX_TOPIC_CHARS {
        return Err(ScraperHttpError::BadRequest(format!(
            "topic must be at most {MAX_TOPIC_CHARS} characters"
        )));
    }
    let limit = request.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(ScraperHttpError::BadRequest(format!(
            "limit must be between 1 and {MAX_LIMIT}"
        )));
    }

    let results = providers::search(&state.client, &state.config, &topic, limit)
        .await
        .map_err(ScraperHttpError::from)?;
    let discovered = results.len();
    let mut documents = Vec::with_capacity(discovered);
    let mut enqueued = 0;
    let mut duplicates = 0;
    let mut skipped = 0_usize;
    for result in results {
        match queue_result(&state, result).await? {
            Some(document) if document.status == "duplicate" => {
                duplicates += 1;
                documents.push(document);
            }
            Some(document) => {
                enqueued += 1;
                documents.push(document);
            }
            None => skipped += 1,
        }
    }
    if let Err(error) = state
        .metrics
        .record(
            u64::try_from(discovered).unwrap_or(u64::MAX),
            u64::try_from(enqueued).unwrap_or(u64::MAX),
            u64::try_from(skipped).unwrap_or(u64::MAX),
            started.elapsed(),
        )
        .await
    {
        eprintln!("ohara-scraper: metrics write failed: {error}");
    }
    Ok(Json(ScrapeResponse {
        topic,
        requested: limit,
        discovered,
        enqueued,
        duplicates,
        documents,
    }))
}

async fn queue_result(
    state: &AppState,
    result: TopicResult,
) -> Result<Option<QueuedDocument>, ScraperHttpError> {
    let document_id = id_for_url(&result.url);
    let catalog_path = state
        .data_dir
        .join("catalog")
        .join(format!("{document_id}.json"));
    if tokio::fs::try_exists(&catalog_path).await? {
        return Ok(Some(QueuedDocument {
            document_id,
            job_id: None,
            source_url: result.url,
            status: "duplicate",
            title: result.title,
        }));
    }
    let Some(body) = providers::fetch(&state.client, &state.config, &result.url).await? else {
        return Ok(None);
    };
    let raw_path = state
        .data_dir
        .join("raw")
        .join(format!("{document_id}.html"));
    atomic_write(&raw_path, &body).await?;
    let artifact = RawArtifact {
        schema_version: ARTIFACT_VERSION,
        document_id: document_id.clone(),
        source_url: result.url.clone(),
        title: result.title.clone(),
        raw_path: format!("raw/{document_id}.html"),
        fetched_at: timestamp(),
    };
    atomic_json(
        &state
            .data_dir
            .join("raw")
            .join(format!("{document_id}.json")),
        &artifact,
    )
    .await?;
    let catalog = serde_json::json!({
        "id": document_id,
        "sourceUrl": result.url,
        "title": result.title,
        "status": "NEW",
        "chunkCount": 0,
        "createdAt": artifact.fetched_at,
        "lastProcessedAt": serde_json::Value::Null,
        "error": serde_json::Value::Null
    });
    atomic_json(&catalog_path, &catalog).await?;
    atomic_json(
        &state
            .data_dir
            .join("inbox/cleaning")
            .join(format!("{document_id}.json")),
        &artifact,
    )
    .await?;
    let queued_id = artifact.document_id.clone();
    Ok(Some(QueuedDocument {
        document_id: queued_id.clone(),
        job_id: Some(queued_id),
        source_url: artifact.source_url,
        status: "enqueued",
        title: artifact.title,
    }))
}

#[derive(Debug, thiserror::Error)]
enum ScraperHttpError {
    #[error("process authentication failed")]
    Unauthorized,
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Scraper(#[from] ScraperError),
    #[error("{0}")]
    Storage(#[from] std::io::Error),
}

impl IntoResponse for ScraperHttpError {
    fn into_response(self) -> axum::response::Response {
        let message = self.to_string();
        let status = match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Scraper(_) | Self::Storage(_) => StatusCode::BAD_GATEWAY,
        };
        let mut response = (status, Json(serde_json::json!({ "error": message }))).into_response();
        if status == StatusCode::UNAUTHORIZED {
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                axum::http::HeaderValue::from_static("Bearer"),
            );
        }
        response
    }
}

fn authorized(headers: &HeaderMap, expected: &str) -> bool {
    let Some(value) = headers.get(header::AUTHORIZATION) else {
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    let Some((scheme, token)) = value.split_once(' ') else {
        return false;
    };
    scheme.eq_ignore_ascii_case("Bearer") && bool::from(expected.as_bytes().ct_eq(token.as_bytes()))
}

pub(crate) fn normalize_url(raw: &str) -> Result<String, ScraperError> {
    let mut parsed =
        url::Url::parse(raw).map_err(|error| ScraperError::InvalidResponse(error.to_string()))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(ScraperError::InvalidResponse(
            "result URL is not fetchable".into(),
        ));
    }
    parsed.set_fragment(None);
    let retained: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(key, _)| !key.to_ascii_lowercase().starts_with("utm_") && key != "fbclid")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    parsed.set_query(None);
    if !retained.is_empty() {
        parsed.query_pairs_mut().extend_pairs(retained);
    }
    Ok(parsed.to_string())
}

fn id_for_url(url: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(url.as_bytes());
    let digest = digest.finalize();
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

fn data_dir() -> PathBuf {
    std::env::var_os("OHARA_DATA_DIR").map_or_else(|| PathBuf::from("data"), PathBuf::from)
}

fn process_auth_token() -> Option<String> {
    std::env::var(PROCESS_AUTH_ENV)
        .ok()
        .filter(|token| !token.trim().is_empty())
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
        "dead-letter/cleaning",
        "dead-letter/indexer",
        "dead-letter/graph",
        "state",
    ] {
        tokio::fs::create_dir_all(data_dir.join(relative)).await?;
    }
    Ok(())
}

async fn heartbeat(data_dir: &Path) -> Result<(), std::io::Error> {
    atomic_write(
        &data_dir.join("state/scraper.heartbeat"),
        timestamp().as_bytes(),
    )
    .await
}

async fn atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<(), std::io::Error> {
    let bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    atomic_write(path, &bytes).await
}

async fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    tokio::fs::write(&temporary, bytes).await?;
    tokio::fs::rename(temporary, path).await
}

fn timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or_else(|_| "0".into(), |duration| duration.as_secs().to_string())
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("ohara-scraper: failed to install shutdown handler: {error}");
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, header};

    use super::authorized;

    #[test]
    fn authorization_accepts_only_a_matching_bearer_token() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer secret"),
        );
        assert!(authorized(&headers, "secret"));
        assert!(!authorized(&headers, "different"));
    }

    #[test]
    fn authorization_rejects_missing_or_malformed_headers() {
        let headers = HeaderMap::new();
        assert!(!authorized(&headers, "secret"));

        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Basic secret"),
        );
        assert!(!authorized(&headers, "secret"));
    }
}
