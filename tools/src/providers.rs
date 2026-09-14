//! Local HTTP and Redis provider Adapters used by deterministic tools.

use crate::{Error, Result, metrics};
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;

const VECTOR_DIMENSION: usize = 384;

/// A local HTTP server with graceful shutdown.
pub(crate) struct LocalServer {
    port: u16,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<std::result::Result<(), std::io::Error>>>,
}

impl LocalServer {
    async fn start(router: Router) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|source| Error::io("bind provider fixture", source))?;
        let port = listener
            .local_addr()
            .map_err(|source| Error::io("read provider fixture address", source))?
            .port();
        let (shutdown, receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = receiver.await;
                })
                .await
        });
        Ok(Self {
            port,
            shutdown: Some(shutdown),
            task: Some(task),
        })
    }

    /// Return the loopback port assigned to the server.
    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    /// Stop the server and wait for its task to finish.
    pub(crate) async fn shutdown(&mut self) -> Result<()> {
        if let Some(sender) = self.shutdown.take() {
            let _ = sender.send(());
        }
        if let Some(task) = self.task.take() {
            task.await?
                .map_err(|source| Error::io("provider server task", source))?;
        }
        Ok(())
    }
}

/// State and handle for the RSS/HTML fixture server.
pub(crate) struct FixtureServer {
    server: LocalServer,
}

impl FixtureServer {
    /// Return the server's loopback port.
    pub(crate) fn port(&self) -> u16 {
        self.server.port()
    }

    /// Stop the fixture server.
    pub(crate) async fn shutdown(&mut self) -> Result<()> {
        self.server.shutdown().await
    }
}

/// Start the one-document RSS and HTML fixture.
pub(crate) async fn start_fixture_server() -> Result<FixtureServer> {
    let state = Arc::new(FixtureState {
        article_port: Arc::new(Mutex::new(0)),
    });
    let router = Router::new()
        .route("/news", get(fixture_news))
        .route("/article.html", get(fixture_article))
        .with_state(state.clone());
    let server = LocalServer::start(router).await?;
    *state.article_port.lock().await = server.port();
    Ok(FixtureServer { server })
}

struct FixtureState {
    article_port: Arc<Mutex<u16>>,
}

async fn fixture_news(State(state): State<Arc<FixtureState>>) -> impl IntoResponse {
    let port = *state.article_port.lock().await;
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<rss version=\"2.0\"><channel><title>Ohara fixture feed</title><item><title>Ohara fixture article</title><link>http://127.0.0.1:{port}/article.html</link></item></channel></rss>"
    );
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/rss+xml")],
        body,
    )
}

async fn fixture_article() -> impl IntoResponse {
    let body = "<!doctype html>
<html><head><title>Ohara fixture article</title></head>
<body><article>
<h1>Ohara fixture article</h1>
<p>Ohara is a private local knowledge base that turns web topics into searchable evidence.</p>
<p>The scraper fetches source pages, cleaning extracts the article, and the indexer creates searchable chunks.</p>
<p>Retrieval finds the relevant evidence and returns a local grounded answer with citations.</p>
</article></body></html>";
    (StatusCode::OK, [(header::CONTENT_TYPE, "text/html")], body)
}

#[derive(Clone)]
struct QdrantState {
    points: Arc<Mutex<Vec<Value>>>,
    search_mode: Option<String>,
    ranked: bool,
}

/// State and handle for a Qdrant HTTP fixture.
pub(crate) struct QdrantServer {
    server: LocalServer,
    state: QdrantState,
}

impl QdrantServer {
    /// Return the server's loopback port.
    pub(crate) fn port(&self) -> u16 {
        self.server.port()
    }

    /// Add points to the fixture before starting a real process.
    pub(crate) async fn add_points(&self, points: impl IntoIterator<Item = Value>) {
        self.state.points.lock().await.extend(points);
    }

    /// Return the current point count.
    pub(crate) async fn point_count(&self) -> usize {
        self.state.points.lock().await.len()
    }

    /// Stop the fixture server.
    pub(crate) async fn shutdown(&mut self) -> Result<()> {
        self.server.shutdown().await
    }
}

/// Start an insertion-order Qdrant fixture.
pub(crate) async fn start_qdrant_server(search_mode: Option<&str>) -> Result<QdrantServer> {
    start_qdrant(search_mode, false).await
}

/// Start a Qdrant fixture that ranks points by cosine similarity.
pub(crate) async fn start_quality_qdrant_server() -> Result<QdrantServer> {
    start_qdrant(None, true).await
}

async fn start_qdrant(search_mode: Option<&str>, ranked: bool) -> Result<QdrantServer> {
    let state = QdrantState {
        points: Arc::new(Mutex::new(Vec::new())),
        search_mode: search_mode.map(str::to_owned),
        ranked,
    };
    let router = Router::new()
        .route("/collections", get(qdrant_collections))
        .route(
            "/collections/{*path}",
            get(qdrant_get)
                .put(qdrant_put)
                .post(qdrant_post)
                .delete(qdrant_delete),
        )
        .with_state(state.clone());
    let server = LocalServer::start(router).await?;
    Ok(QdrantServer { server, state })
}

async fn qdrant_collections() -> Json<Value> {
    Json(json!({"result": {"collections": []}, "status": "ok"}))
}

async fn qdrant_get(Path(_path): Path<String>) -> Json<Value> {
    Json(json!({
        "result": {"config": {"params": {"vectors": {"size": VECTOR_DIMENSION, "distance": "Cosine"}}}},
        "status": "ok"
    }))
}

async fn qdrant_delete(Path(_path): Path<String>) -> Json<Value> {
    Json(json!({"result": true, "status": "ok"}))
}

async fn qdrant_put(
    State(state): State<QdrantState>,
    Path(path): Path<String>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    if path.ends_with("/points") {
        let Some(points) = payload.get("points").and_then(Value::as_array) else {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"status": "invalid fixture vector"})),
            );
        };
        if points.iter().any(|point| {
            point
                .get("vector")
                .and_then(Value::as_array)
                .is_none_or(|vector| vector.len() != VECTOR_DIMENSION)
        }) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"status": "invalid fixture vector"})),
            );
        }
        state.points.lock().await.extend(points.iter().cloned());
    }
    (
        StatusCode::OK,
        Json(json!({"result": true, "status": "ok"})),
    )
}

async fn qdrant_post(
    State(state): State<QdrantState>,
    Path(path): Path<String>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    if !path.ends_with("/points/search") {
        return (StatusCode::NOT_FOUND, Json(json!({"status": "not found"})));
    }
    let Some(vector) = payload.get("vector").and_then(Value::as_array) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"status": "invalid fixture query vector"})),
        );
    };
    if vector.len() != VECTOR_DIMENSION {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"status": "invalid fixture query vector"})),
        );
    }
    if let Some(expected_mode) = state.search_mode.as_deref() {
        let expected = if expected_mode == "exact" {
            json!({"exact": true})
        } else {
            json!({"exact": false, "hnsw_ef": 64})
        };
        if payload.get("params") != Some(&expected) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"status": "search mode parameters were not explicit"})),
            );
        }
    }
    let requested = payload
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(usize::MAX, |value| {
            usize::try_from(value).unwrap_or(usize::MAX)
        });
    let mut points = state.points.lock().await.clone();
    if state.ranked {
        let query: Vec<f64> = vector.iter().filter_map(Value::as_f64).collect();
        points.sort_by(|left, right| {
            let left_score = point_score(&query, left);
            let right_score = point_score(&query, right);
            right_score
                .total_cmp(&left_score)
                .then_with(|| point_id(left).cmp(&point_id(right)))
        });
    }
    let result = points
        .into_iter()
        .take(requested)
        .map(|point| {
            json!({
                "id": point.get("id").cloned().unwrap_or(Value::Null),
                "score": point_score(&vector.iter().filter_map(Value::as_f64).collect::<Vec<_>>(), &point),
                "payload": point.get("payload").cloned().unwrap_or_else(|| json!({}))
            })
        })
        .collect::<Vec<_>>();
    (
        StatusCode::OK,
        Json(json!({"result": result, "status": "ok"})),
    )
}

fn point_id(point: &Value) -> String {
    point
        .get("id")
        .map_or_else(String::new, ToString::to_string)
}

fn point_score(query: &[f64], point: &Value) -> f64 {
    let values = point
        .get("vector")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_f64).collect::<Vec<_>>())
        .unwrap_or_default();
    metrics::cosine(query, &values)
}

/// State and handle for the Ollama response fixture.
pub(crate) struct OllamaServer {
    server: LocalServer,
    response_mode: Arc<Mutex<OllamaResponseMode>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OllamaResponseMode {
    Grounded,
    Empty,
    Unavailable,
    Malformed,
}

impl OllamaServer {
    /// Return the server's loopback port.
    pub(crate) fn port(&self) -> u16 {
        self.server.port()
    }

    /// Change the response used by subsequent generation requests.
    pub(crate) async fn set_response_mode(&self, mode: &str) -> Result<()> {
        let parsed = match mode {
            "grounded" => OllamaResponseMode::Grounded,
            "empty" => OllamaResponseMode::Empty,
            "unavailable" => OllamaResponseMode::Unavailable,
            "malformed" => OllamaResponseMode::Malformed,
            other => {
                return Err(Error::Message(format!(
                    "unknown Ollama fixture mode {other}"
                )));
            }
        };
        *self.response_mode.lock().await = parsed;
        Ok(())
    }

    /// Stop the fixture server.
    pub(crate) async fn shutdown(&mut self) -> Result<()> {
        self.server.shutdown().await
    }
}

/// Start an Ollama tags and generation fixture.
pub(crate) async fn start_ollama_server() -> Result<OllamaServer> {
    let response_mode = Arc::new(Mutex::new(OllamaResponseMode::Grounded));
    let router = Router::new()
        .route("/api/tags", get(ollama_tags))
        .route("/api/generate", post(ollama_generate))
        .with_state(response_mode.clone());
    let server = LocalServer::start(router).await?;
    Ok(OllamaServer {
        server,
        response_mode,
    })
}

async fn ollama_tags() -> Json<Value> {
    Json(json!({"models": [{"name": "fixture"}]}))
}

async fn ollama_generate(
    State(response_mode): State<Arc<Mutex<OllamaResponseMode>>>,
    Json(_payload): Json<Value>,
) -> impl IntoResponse {
    match *response_mode.lock().await {
        OllamaResponseMode::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "fixture Ollama unavailable"})),
        ),
        OllamaResponseMode::Malformed => (StatusCode::OK, Json(json!({"response": 17}))),
        OllamaResponseMode::Empty => (StatusCode::OK, Json(json!({"response": "  "}))),
        OllamaResponseMode::Grounded => (
            StatusCode::OK,
            Json(
                json!({"response": "Ohara is a private local knowledge base built from local artifacts."}),
            ),
        ),
    }
}

/// Start a benchmark RSS/HTML feed for representative documents.
pub(crate) async fn start_benchmark_feed(
    documents: Vec<crate::workload::CorpusDocument>,
) -> Result<FixtureServer> {
    let state = Arc::new(BenchmarkFeedState {
        documents,
        port: Arc::new(Mutex::new(0)),
    });
    let router = Router::new()
        .route("/news", get(benchmark_news))
        .route("/article/{*path}", get(benchmark_article))
        .with_state(state.clone());
    let server = LocalServer::start(router).await?;
    *state.port.lock().await = server.port();
    Ok(FixtureServer { server })
}

struct BenchmarkFeedState {
    documents: Vec<crate::workload::CorpusDocument>,
    port: Arc<Mutex<u16>>,
}

async fn benchmark_news(State(state): State<Arc<BenchmarkFeedState>>) -> impl IntoResponse {
    let port = *state.port.lock().await;
    let items = state.documents.iter().enumerate().fold(
        String::new(),
        |mut output, (index, document)| {
            use std::fmt::Write as _;
            let _ = write!(
                output,
                "<item><title>{}</title><link>http://127.0.0.1:{port}/article/{index}.html</link></item>",
                xml_escape(&document.title)
            );
            output
        },
    );
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><rss version=\"2.0\"><channel>{items}</channel></rss>"
    );
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/rss+xml")],
        body,
    )
}

async fn benchmark_article(
    State(state): State<Arc<BenchmarkFeedState>>,
    Path(path): Path<String>,
) -> impl IntoResponse {
    let index = path
        .strip_suffix(".html")
        .and_then(|value| value.parse::<usize>().ok());
    state
        .documents
        .get(index.unwrap_or(usize::MAX))
        .map_or_else(
            || {
                (
                    StatusCode::NOT_FOUND,
                    [(header::CONTENT_TYPE, "text/plain")],
                    String::new(),
                )
            },
            |document| {
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "text/html")],
                    document.html.clone(),
                )
            },
        )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Small RESP server sufficient for graph and health checks.
pub(crate) struct FakeRedisServer {
    port: u16,
    query_count: Arc<std::sync::atomic::AtomicUsize>,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl FakeRedisServer {
    /// Start the local RESP fixture.
    pub(crate) async fn start() -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|source| Error::io("bind fake FalkorDB", source))?;
        let port = listener
            .local_addr()
            .map_err(|source| Error::io("read fake FalkorDB address", source))?
            .port();
        let query_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = query_count.clone();
        let (shutdown, mut receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    result = listener.accept() => {
                        let Ok((stream, _)) = result else { break };
                        let count = count.clone();
                        tokio::spawn(async move { let _ = handle_redis_connection(stream, count).await; });
                    }
                    _ = &mut receiver => break,
                }
            }
        });
        Ok(Self {
            port,
            query_count,
            shutdown: Some(shutdown),
            task: Some(task),
        })
    }

    /// Return the loopback port.
    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    /// Return the number of graph queries accepted.
    pub(crate) fn query_count(&self) -> usize {
        self.query_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Stop the RESP fixture.
    pub(crate) async fn shutdown(&mut self) -> Result<()> {
        if let Some(sender) = self.shutdown.take() {
            let _ = sender.send(());
        }
        if let Some(task) = self.task.take() {
            task.await?;
        }
        Ok(())
    }
}

async fn handle_redis_connection(
    stream: TcpStream,
    query_count: Arc<std::sync::atomic::AtomicUsize>,
) -> Result<()> {
    let mut reader = BufReader::new(stream);
    loop {
        let Some(command) = read_resp_command(&mut reader).await? else {
            return Ok(());
        };
        let name = command
            .first()
            .map(|value| value.to_ascii_uppercase())
            .unwrap_or_default();
        let response = if name == b"HELLO" {
            b"%2\r\n$6\r\nserver\r\n$5\r\nredis\r\n$7\r\nversion\r\n$3\r\n7.2\r\n".to_vec()
        } else if name == b"PING" {
            b"+PONG\r\n".to_vec()
        } else {
            if name == b"GRAPH.QUERY" {
                query_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            b"+OK\r\n".to_vec()
        };
        reader
            .get_mut()
            .write_all(&response)
            .await
            .map_err(|source| Error::io("write fake FalkorDB response", source))?;
    }
}

async fn read_resp_command(reader: &mut BufReader<TcpStream>) -> Result<Option<Vec<Vec<u8>>>> {
    let mut header = Vec::new();
    if reader
        .read_until(b'\n', &mut header)
        .await
        .map_err(|source| Error::io("read fake Redis command", source))?
        == 0
    {
        return Ok(None);
    }
    if !header.starts_with(b"*") {
        return Err(Error::Message(
            "fake Redis expected an array command".to_owned(),
        ));
    }
    let count = parse_resp_number(&header, b"*")?;
    let mut command = Vec::with_capacity(count);
    for _ in 0..count {
        let mut length_header = Vec::new();
        reader
            .read_until(b'\n', &mut length_header)
            .await
            .map_err(|source| Error::io("read fake Redis argument length", source))?;
        if !length_header.starts_with(b"$") {
            return Err(Error::Message(
                "fake Redis expected bulk command arguments".to_owned(),
            ));
        }
        let length = parse_resp_number(&length_header, b"$")?;
        let mut value = vec![0; length];
        reader
            .read_exact(&mut value)
            .await
            .map_err(|source| Error::io("read fake Redis argument", source))?;
        let mut terminator = [0; 2];
        reader
            .read_exact(&mut terminator)
            .await
            .map_err(|source| Error::io("read fake Redis argument terminator", source))?;
        if terminator != *b"\r\n" {
            return Err(Error::Message(
                "fake Redis argument was truncated".to_owned(),
            ));
        }
        command.push(value);
    }
    Ok(Some(command))
}

fn parse_resp_number(line: &[u8], prefix: &[u8]) -> Result<usize> {
    let value = line
        .strip_prefix(prefix)
        .and_then(|value| value.strip_suffix(b"\r\n"))
        .ok_or_else(|| Error::Message("fake Redis malformed integer".to_owned()))?;
    std::str::from_utf8(value)
        .map_err(|_| Error::Message("fake Redis integer was not UTF-8".to_owned()))?
        .parse::<usize>()
        .map_err(|_| Error::Message("fake Redis integer was invalid".to_owned()))
}
