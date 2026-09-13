//! Graph process.
//!
//! This process consumes the canonical indexed artifact and publishes a small,
//! idempotent entity/mention graph to `FalkorDB`. Entity extraction is deliberately
//! bounded and local; replacing it with an LLM adapter does not change the
//! artifact seam.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use sha2::{Digest, Sha256};

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

    let mut interval = tokio::time::interval(POLL_INTERVAL);
    loop {
        tokio::select! {
            _ = interval.tick() => process_pending(&data_dir, &mut connection, &metrics).await?,
            result = tokio::signal::ctrl_c() => {
                if let Err(error) = result {
                    eprintln!("ohara-graph: shutdown signal failed: {error}");
                }
                return Ok(());
            }
        }
    }
}

async fn process_pending<C>(
    data_dir: &Path,
    connection: &mut C,
    metrics: &metrics::Metrics,
) -> Result<(), GraphError>
where
    C: redis::aio::ConnectionLike + Send + Sync,
{
    let mut entries = tokio::fs::read_dir(data_dir.join("inbox/graph")).await?;
    while let Some(entry) = entries.next_entry().await? {
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
    Ok(())
}

async fn process_one<C>(path: &Path, connection: &mut C) -> Result<(), GraphError>
where
    C: redis::aio::ConnectionLike + Send + Sync,
{
    let artifact: IndexedArtifact = decode(&tokio::fs::read(path).await?)?;
    validate_version(artifact.schema_version)?;
    for chunk in &artifact.chunks {
        for entity in entities(&chunk.text) {
            let query = format!(
                "MERGE (c:Chunk {{id:'{}', doc_id:'{}'}}) MERGE (e:Entity {{id:'{}', name:'{}'}}) MERGE (c)-[:MENTIONS]->(e)",
                escape(&chunk.chunk_id),
                escape(&artifact.document_id),
                escape(&entity_id(&entity)),
                escape(&entity),
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

fn entities(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = Vec::new();
    for word in text.split_whitespace() {
        let clean = word.trim_matches(|character: char| !character.is_alphanumeric());
        let starts_uppercase = clean.chars().next().is_some_and(char::is_uppercase);
        if starts_uppercase && clean.chars().count() > 2 {
            current.push(clean.to_string());
        } else if !current.is_empty() {
            result.push(current.join(" "));
            current.clear();
        }
    }
    if !current.is_empty() {
        result.push(current.join(" "));
    }
    result.sort_unstable();
    result.dedup();
    result.truncate(32);
    result
}

fn entity_id(name: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(name.to_ascii_lowercase().as_bytes());
    format!("entity-{}", hex(&digest.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
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
