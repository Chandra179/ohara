//! Graph process.
//!
//! This process consumes the canonical indexed artifact and publishes a small,
//! idempotent typed entity/mention graph to `FalkorDB`. Extraction and identity
//! resolution are deterministic, bounded, and local; neither changes the
//! artifact seam.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use tokio::sync::watch;

mod entities;
mod entity_resolution;
mod metrics;

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const ARTIFACT_VERSION: u8 = 1;

/// Errors raised by the graph process.
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    /// An indexed artifact could not be decoded.
    #[error("invalid artifact: {0}")]
    Artifact(String),
    /// A local file operation failed.
    #[error("storage: {0}")]
    Storage(#[from] std::io::Error),
    /// `FalkorDB` could not accept the graph write.
    #[error("falkordb: {0}")]
    Falkor(String),
}

#[derive(Debug, Deserialize)]
struct IndexedArtifact {
    schema_version: u8,
    document_id: String,
    source_url: String,
    title: String,
    chunks: Vec<Chunk>,
}

#[derive(Debug, Deserialize)]
struct Chunk {
    chunk_id: String,
    text: String,
}

/// Runs graph extraction until interrupted.
///
/// # Errors
///
/// Returns an error when the artifact directory or `FalkorDB` connection cannot
/// be prepared, or when a graph write fails.
pub async fn run() -> Result<(), GraphError> {
    let data_dir = data_dir();
    ensure_layout(&data_dir).await?;
    heartbeat(&data_dir).await?;
    let client = redis::Client::open(falkordb_url())
        .map_err(|error| GraphError::Falkor(error.to_string()))?;
    let mut connection = client
        .get_multiplexed_async_connection()
        .await
        .map_err(|error| GraphError::Falkor(error.to_string()))?;
    let metrics = metrics::Metrics::open(&data_dir, "graph").await?;
    let heartbeat_dir = data_dir.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            if let Err(error) = heartbeat(&heartbeat_dir).await {
                eprintln!("ohara-graph: heartbeat failed: {error}");
            }
        }
    });
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        let _ = shutdown_tx.send(true);
    });

    let mut interval = tokio::time::interval(POLL_INTERVAL);
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            result = shutdown_rx.changed() => {
                if result.is_ok() {
                    eprintln!("ohara-graph: shutdown requested; stopping new work");
                    eprintln!("ohara-graph: drain complete");
                }
                return Ok(());
            }
        }
        process_pending(&data_dir, &mut connection, &metrics, shutdown_rx.clone()).await?;
        if *shutdown_rx.borrow() {
            eprintln!("ohara-graph: drain complete");
            return Ok(());
        }
    }
}

/// Runs the deterministic entity-resolution threshold benchmark.
///
/// The benchmark reads the versioned labeled fixture from the repository and
/// reports precision, recall, F1, false merges, and missed merges for every
/// configured candidate threshold. It does not contact `FalkorDB` or mutate
/// runtime data.
///
/// # Errors
///
/// Returns an error when the fixture is missing, invalid, or fails its
/// documented operating-threshold regression gate.
pub fn run_entity_resolution_benchmark() -> Result<(), GraphError> {
    entity_resolution::run("docs/architecture/fixtures/entity-resolution-v1.json")
        .map_err(GraphError::Artifact)
}

async fn process_pending<C>(
    data_dir: &Path,
    connection: &mut C,
    metrics: &metrics::Metrics,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), GraphError>
where
    C: redis::aio::ConnectionLike + Send + Sync,
{
    let mut entries = tokio::fs::read_dir(data_dir.join("inbox/graph")).await?;
    loop {
        if *shutdown.borrow() {
            return Ok(());
        }
        let entry = tokio::select! {
            result = entries.next_entry() => result?,
            result = shutdown.changed() => {
                if result.is_err() {
                    return Ok(());
                }
                return Ok(());
            }
        };
        let Some(entry) = entry else {
            return Ok(());
        };
        if *shutdown.borrow() {
            return Ok(());
        }
        if !entry.file_type().await?.is_file()
            || entry.path().extension().is_none_or(|ext| ext != "json")
        {
            continue;
        }
        let path = entry.path();
        let started = Instant::now();
        let result = process_one(&path, connection).await;
        let succeeded = result.is_ok();
        if let Err(error) = metrics
            .record(
                1,
                u64::from(succeeded),
                u64::from(!succeeded),
                started.elapsed(),
            )
            .await
        {
            eprintln!("ohara-graph: metrics write failed: {error}");
        }
        if let Err(error) = &result {
            eprintln!("ohara-graph: {}: {error}", path.display());
            dead_letter(data_dir, &path).await?;
            continue;
        }
        tokio::fs::remove_file(path).await?;
    }
}

async fn process_one<C>(path: &Path, connection: &mut C) -> Result<(), GraphError>
where
    C: redis::aio::ConnectionLike + Send + Sync,
{
    let artifact: IndexedArtifact = decode(&tokio::fs::read(path).await?)?;
    validate_version(artifact.schema_version)?;
    for chunk in &artifact.chunks {
        for entity in entities::extract(&chunk.text) {
            let query = format!(
                "MERGE (c:Chunk {{id:'{}', doc_id:'{}'}}) MERGE (e:Entity {{id:'{}'}}) SET e.name='{}', e.type='{}', e.normalized='{}', e.aliases='{}' MERGE (c)-[:MENTIONS]->(e)",
                escape(&chunk.chunk_id),
                escape(&artifact.document_id),
                escape(&entity.id()),
                escape(&entity.name),
                entity.kind.as_str(),
                escape(&entity.normalized),
                escape(&entity.aliases.join("|")),
            );
            let _: redis::Value = redis::cmd("GRAPH.QUERY")
                .arg(graph_name())
                .arg(query)
                .query_async(connection)
                .await
                .map_err(|error| GraphError::Falkor(error.to_string()))?;
        }
    }
    let _ = (&artifact.source_url, &artifact.title);
    Ok(())
}

fn validate_version(version: u8) -> Result<(), GraphError> {
    (version == ARTIFACT_VERSION).then_some(()).ok_or_else(|| {
        GraphError::Artifact(format!(
            "unsupported indexed artifact schema version {version}; expected {ARTIFACT_VERSION}"
        ))
    })
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}
fn data_dir() -> PathBuf {
    std::env::var_os("OHARA_DATA_DIR").map_or_else(|| PathBuf::from("data"), PathBuf::from)
}
fn falkordb_url() -> String {
    std::env::var("OHARA_FALKORDB_URL").unwrap_or_else(|_| "redis://127.0.0.1:6380".into())
}
fn graph_name() -> String {
    std::env::var("OHARA_FALKORDB_GRAPH").unwrap_or_else(|_| "ohara".into())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut interrupt =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()) {
                Ok(signal) => signal,
                Err(error) => {
                    eprintln!("ohara-graph: failed to install SIGINT handler: {error}");
                    if let Err(error) = tokio::signal::ctrl_c().await {
                        eprintln!("ohara-graph: shutdown signal failed: {error}");
                    }
                    return;
                }
            };
        let mut terminate =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(error) => {
                    eprintln!("ohara-graph: failed to install SIGTERM handler: {error}");
                    if let Err(error) = tokio::signal::ctrl_c().await {
                        eprintln!("ohara-graph: shutdown signal failed: {error}");
                    }
                    return;
                }
            };
        tokio::select! {
            _ = interrupt.recv() => {},
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("ohara-graph: shutdown signal failed: {error}");
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
        "dead-letter/cleaning",
        "dead-letter/indexer",
        "dead-letter/graph",
        "state",
        "models",
    ] {
        tokio::fs::create_dir_all(data_dir.join(relative)).await?;
    }
    Ok(())
}

async fn dead_letter(data_dir: &Path, path: &Path) -> Result<(), std::io::Error> {
    let filename = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("inbox path has no filename"))?;
    tokio::fs::rename(path, data_dir.join("dead-letter/graph").join(filename)).await
}
async fn heartbeat(data_dir: &Path) -> Result<(), std::io::Error> {
    atomic_write(
        &data_dir.join("state/graph.heartbeat"),
        timestamp().as_bytes(),
    )
    .await
}
fn decode<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, GraphError> {
    serde_json::from_slice(bytes).map_err(|error| GraphError::Artifact(error.to_string()))
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
        .map_or_else(|_| "0".into(), |value| value.as_secs().to_string())
}

#[cfg(test)]
mod tests {
    use super::{ARTIFACT_VERSION, GraphError, IndexedArtifact, validate_version};

    #[test]
    fn indexed_v1_fixture_matches_the_input_contract() {
        let artifact = serde_json::from_str::<IndexedArtifact>(include_str!(
            "../../docs/architecture/fixtures/indexed-v1.json"
        ));
        assert!(artifact.is_ok());
    }

    #[test]
    fn rejects_unknown_indexed_artifact_versions() {
        assert!(matches!(
            validate_version(ARTIFACT_VERSION + 1),
            Err(GraphError::Artifact(message)) if message.contains("schema version")
        ));
    }
}
