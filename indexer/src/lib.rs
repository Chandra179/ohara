//! Indexer process.
//!
//! The indexer is the sole owner of canonical chunking and vector publication.
//! It consumes clean artifacts, writes the indexed artifact used by retrieval,
//! publishes vectors to Qdrant, and hands the same chunks to graph extraction.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod metrics;

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const EMBEDDING_DIMENSION: usize = 384;
const MAX_CHUNK_CHARS: usize = 2_400;
const OVERLAP_CHARS: usize = 240;
const ARTIFACT_VERSION: u8 = 1;

/// Errors raised by the indexer process.
#[derive(Debug, thiserror::Error)]
pub enum IndexerError {
    /// A clean artifact could not be decoded.
    #[error("invalid artifact: {0}")]
    Artifact(String),
    /// A local file operation failed.
    #[error("storage: {0}")]
    Storage(#[from] std::io::Error),
    /// The local embedding model could not be loaded or used.
    #[error("embedder: {0}")]
    Embedder(String),
    /// Qdrant rejected a request or was unavailable.
    #[error("qdrant: {0}")]
    Qdrant(String),
}

#[derive(Debug, Deserialize)]
struct CleanArtifact {
    schema_version: u8,
    document_id: String,
    source_url: String,
    title: String,
    markdown: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Chunk {
    /// Stable chunk identity derived from document identity and sequence.
    #[serde(rename = "chunk_id")]
    pub id: String,
    /// Source document identity.
    pub document_id: String,
    /// Human-readable source title.
    pub title: String,
    /// Source URL.
    pub source_url: String,
    /// Text stored in the vector payload and returned as evidence.
    pub text: String,
    /// Position within the canonical chunk sequence.
    pub sequence: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct IndexedArtifact {
    /// Version of the indexed artifact contract.
    pub schema_version: u8,
    /// Stable document identity.
    pub document_id: String,
    /// Source URL.
    pub source_url: String,
    /// Source title.
    pub title: String,
    /// Canonical chunks.
    pub chunks: Vec<Chunk>,
    /// Index completion timestamp.
    pub indexed_at: String,
}

enum Embedder {
    FastEmbed(Box<Mutex<TextEmbedding>>),
    Deterministic,
}

impl Embedder {
    fn load(cache_dir: &Path) -> Result<Self, IndexerError> {
        if std::env::var("OHARA_EMBEDDING_MODE").as_deref() == Ok("deterministic") {
            return Ok(Self::Deterministic);
        }
        std::fs::create_dir_all(cache_dir)?;
        let model = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::BGESmallENV15)
                .with_cache_dir(cache_dir.to_path_buf())
                .with_max_length(512)
                .with_show_download_progress(false),
        )
        .map_err(|error| IndexerError::Embedder(error.to_string()))?;
        Ok(Self::FastEmbed(Box::new(Mutex::new(model))))
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, IndexerError> {
        if matches!(self, Self::Deterministic) {
            return Ok(texts
                .iter()
                .map(|text| deterministic_embedding(text))
                .collect());
        }
        let references: Vec<&str> = texts.iter().map(String::as_str).collect();
        let Self::FastEmbed(model) = self else {
            return Err(IndexerError::Embedder(
                "embedder was not initialized".into(),
            ));
        };
        model
            .lock()
            .map_err(|_| IndexerError::Embedder("embedder mutex poisoned".into()))?
            .embed(references, None)
            .map_err(|error| IndexerError::Embedder(error.to_string()))
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

/// Runs the indexer worker until interrupted.
///
/// # Errors
///
/// Returns an error when the local model, Qdrant, or artifact directories
/// cannot be prepared.
pub async fn run() -> Result<(), IndexerError> {
    let data_dir = data_dir();
    ensure_layout(&data_dir).await?;
    heartbeat(&data_dir).await?;
    let embedder = Embedder::load(&data_dir.join("models"))?;
    let qdrant = Qdrant::new(qdrant_url())?;
    qdrant.ensure_collection().await?;
    let metrics = metrics::Metrics::open(&data_dir, "indexer").await?;
    let heartbeat_dir = data_dir.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            if let Err(error) = heartbeat(&heartbeat_dir).await {
                eprintln!("ohara-indexer: heartbeat failed: {error}");
            }
        }
    });
    let mut interval = tokio::time::interval(POLL_INTERVAL);
    loop {
        tokio::select! {
            _ = interval.tick() => process_pending(&data_dir, &embedder, &qdrant, &metrics).await?,
            result = tokio::signal::ctrl_c() => {
                if let Err(error) = result {
                    eprintln!("ohara-indexer: shutdown signal failed: {error}");
                }
                return Ok(());
            }
        }
    }
}

async fn process_pending(
    data_dir: &Path,
    embedder: &Embedder,
    qdrant: &Qdrant,
    metrics: &metrics::Metrics,
) -> Result<(), IndexerError> {
    let mut entries = tokio::fs::read_dir(data_dir.join("inbox/indexer")).await?;
    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_file()
            || entry.path().extension().is_none_or(|ext| ext != "json")
        {
            continue;
        }
        let path = entry.path();
        let started = Instant::now();
        let result = process_one(data_dir, &path, embedder, qdrant).await;
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
            eprintln!("ohara-indexer: metrics write failed: {error}");
        }
        if let Err(error) = &result {
            eprintln!("ohara-indexer: {}: {error}", path.display());
            if let Some(id) = path.file_stem().and_then(|value| value.to_str()) {
                update_catalog(data_dir, id, "FAILED", Some(&error.to_string()), 0).await?;
            }
            dead_letter(data_dir, &path).await?;
            continue;
        }
        tokio::fs::remove_file(path).await?;
    }
    Ok(())
}

async fn process_one(
    data_dir: &Path,
    path: &Path,
    embedder: &Embedder,
    qdrant: &Qdrant,
) -> Result<(), IndexerError> {
    let clean: CleanArtifact = decode(&tokio::fs::read(path).await?)?;
    validate_version(clean.schema_version)?;
    let chunks = chunk(&clean);
    let texts: Vec<String> = chunks.iter().map(|item| item.text.clone()).collect();
    let vectors = embedder.embed(&texts)?;
    qdrant.upsert(&chunks, &vectors).await?;
    let count = chunks.len();
    let indexed = IndexedArtifact {
        schema_version: ARTIFACT_VERSION,
        document_id: clean.document_id.clone(),
        source_url: clean.source_url,
        title: clean.title,
        chunks,
        indexed_at: timestamp(),
    };
    atomic_json(
        &data_dir
            .join("indexed")
            .join(format!("{}.json", clean.document_id)),
        &indexed,
    )
    .await?;
    atomic_json(
        &data_dir
            .join("inbox/graph")
            .join(format!("{}.json", clean.document_id)),
        &indexed,
    )
    .await?;
    update_catalog(data_dir, &clean.document_id, "INDEXED", None, count).await
}

fn validate_version(version: u8) -> Result<(), IndexerError> {
    (version == ARTIFACT_VERSION).then_some(()).ok_or_else(|| {
        IndexerError::Artifact(format!(
            "unsupported clean artifact schema version {version}; expected {ARTIFACT_VERSION}"
        ))
    })
}

fn chunk(clean: &CleanArtifact) -> Vec<Chunk> {
    let mut output = Vec::new();
    let mut start = 0;
    let text = clean.markdown.trim();
    while start < text.len() {
        let mut end = (start + MAX_CHUNK_CHARS).min(text.len());
        if end < text.len()
            && let Some(relative) =
                text[start..end].rfind(|character: char| character.is_whitespace())
        {
            end = start + relative;
        }
        if end <= start {
            end = (start + MAX_CHUNK_CHARS).min(text.len());
        }
        let piece = text[start..end].trim();
        if !piece.is_empty() {
            let sequence = output.len();
            output.push(Chunk {
                id: chunk_id(&clean.document_id, sequence),
                document_id: clean.document_id.clone(),
                title: clean.title.clone(),
                source_url: clean.source_url.clone(),
                text: piece.to_string(),
                sequence,
            });
        }
        if end == text.len() {
            break;
        }
        start = end.saturating_sub(OVERLAP_CHARS);
    }
    output
}

fn chunk_id(document_id: &str, sequence: usize) -> String {
    let mut digest = Sha256::new();
    digest.update(format!("{document_id}:{sequence}").as_bytes());
    let digest = digest.finalize();
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

struct Qdrant {
    client: Client,
    base_url: String,
}

impl Qdrant {
    fn new(base_url: String) -> Result<Self, IndexerError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|error| IndexerError::Qdrant(error.to_string()))?;
        Ok(Self { client, base_url })
    }

    async fn ensure_collection(&self) -> Result<(), IndexerError> {
        let response = self.client.put(format!("{}/collections/ohara_chunks", self.base_url)).json(&serde_json::json!({"vectors": {"size": EMBEDDING_DIMENSION, "distance": "Cosine"}})).send().await.map_err(|error| IndexerError::Qdrant(error.to_string()))?;
        if response.status().is_success() || response.status() == reqwest::StatusCode::CONFLICT {
            Ok(())
        } else {
            Err(IndexerError::Qdrant(format!(
                "collection creation returned {}",
                response.status()
            )))
        }
    }

    async fn upsert(&self, chunks: &[Chunk], vectors: &[Vec<f32>]) -> Result<(), IndexerError> {
        if chunks.len() != vectors.len() {
            return Err(IndexerError::Qdrant("chunk/vector count mismatch".into()));
        }
        let points: Vec<serde_json::Value> = chunks.iter().zip(vectors).map(|(chunk, vector)| serde_json::json!({
            "id": point_id(&chunk.id),
            "vector": vector,
            "payload": {"chunkId": chunk.id, "documentId": chunk.document_id, "title": chunk.title, "sourceUrl": chunk.source_url, "text": chunk.text}
        })).collect();
        let response = self
            .client
            .put(format!("{}/collections/ohara_chunks/points", self.base_url))
            .query(&[("wait", "true")])
            .json(&serde_json::json!({"points": points}))
            .send()
            .await
            .map_err(|error| IndexerError::Qdrant(error.to_string()))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(IndexerError::Qdrant(format!(
                "upsert returned {}",
                response.status()
            )))
        }
    }
}

fn point_id(chunk_id: &str) -> String {
    format!(
        "{}-{}-{}-{}-{}",
        &chunk_id[0..8],
        &chunk_id[8..12],
        &chunk_id[12..16],
        &chunk_id[16..20],
        &chunk_id[20..32]
    )
}

async fn update_catalog(
    data_dir: &Path,
    document_id: &str,
    status: &str,
    error: Option<&str>,
    chunks: usize,
) -> Result<(), IndexerError> {
    let path = data_dir.join("catalog").join(format!("{document_id}.json"));
    let mut value: serde_json::Value = if tokio::fs::try_exists(&path).await? {
        decode(&tokio::fs::read(&path).await?)?
    } else {
        serde_json::json!({"id": document_id})
    };
    value["status"] = serde_json::Value::String(status.to_string());
    value["lastProcessedAt"] = serde_json::Value::String(timestamp());
    value["chunkCount"] = serde_json::Value::Number(chunks.into());
    value["error"] = error.map_or(serde_json::Value::Null, |text| {
        serde_json::Value::String(text.to_string())
    });
    atomic_json(&path, &value).await
}

fn data_dir() -> PathBuf {
    std::env::var_os("OHARA_DATA_DIR").map_or_else(|| PathBuf::from("data"), PathBuf::from)
}
fn qdrant_url() -> String {
    std::env::var("OHARA_QDRANT_URL").unwrap_or_else(|_| "http://127.0.0.1:6335".into())
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
    tokio::fs::rename(path, data_dir.join("dead-letter/indexer").join(filename)).await
}
async fn heartbeat(data_dir: &Path) -> Result<(), std::io::Error> {
    atomic_write(
        &data_dir.join("state/indexer.heartbeat"),
        timestamp().as_bytes(),
    )
    .await
}
fn decode<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, IndexerError> {
    serde_json::from_slice(bytes).map_err(|error| IndexerError::Artifact(error.to_string()))
}
async fn atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<(), IndexerError> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| IndexerError::Artifact(error.to_string()))?;
    atomic_write(path, &bytes)
        .await
        .map_err(IndexerError::Storage)
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
    use super::{ARTIFACT_VERSION, CleanArtifact, IndexerError, chunk, validate_version};

    #[test]
    fn clean_v1_fixture_matches_the_input_contract() {
        let artifact = serde_json::from_str::<CleanArtifact>(include_str!(
            "../../docs/architecture/fixtures/clean-v1.json"
        ));
        assert!(artifact.is_ok());
    }

    #[test]
    fn chunks_are_stable_and_have_document_identity() {
        let clean = CleanArtifact {
            schema_version: ARTIFACT_VERSION,
            document_id: "doc".into(),
            source_url: "https://example.com".into(),
            title: "Title".into(),
            markdown: "A paragraph. ".repeat(500),
        };
        let chunks = chunk(&clean);
        assert!(chunks.len() > 1);
        assert!(
            chunks
                .iter()
                .enumerate()
                .all(|(index, item)| item.sequence == index && item.id.len() == 64)
        );
    }

    #[test]
    fn rejects_unknown_clean_artifact_versions() {
        assert!(matches!(
            validate_version(ARTIFACT_VERSION + 1),
            Err(IndexerError::Artifact(message)) if message.contains("schema version")
        ));
    }
}
