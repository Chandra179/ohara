//! Cleaning process.
//!
//! The process consumes raw artifacts from `inbox/cleaning`, extracts the main
//! article body, applies quality normalization, and publishes clean artifacts
//! to `inbox/indexer`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::watch;

mod metrics;

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const MIN_WORDS: usize = 20;
const ARTIFACT_VERSION: u8 = 1;

/// Errors raised by the cleaning process.
#[derive(Debug, thiserror::Error)]
pub enum CleaningError {
    /// An artifact could not be decoded.
    #[error("invalid artifact: {0}")]
    Artifact(String),
    /// A local file operation failed.
    #[error("storage: {0}")]
    Storage(#[from] std::io::Error),
}

#[derive(Debug, Deserialize)]
struct RawArtifact {
    schema_version: u8,
    document_id: String,
    source_url: String,
    title: String,
    raw_path: String,
}

#[derive(Debug, Serialize)]
struct CleanArtifact {
    schema_version: u8,
    document_id: String,
    source_url: String,
    title: String,
    markdown: String,
    language: Option<String>,
    cleaned_at: String,
}

/// Runs the cleaning worker until interrupted.
///
/// # Errors
///
/// Returns an error when the artifact directories cannot be prepared or a
/// queued artifact cannot be processed.
pub async fn run() -> Result<(), CleaningError> {
    let data_dir = data_dir();
    ensure_layout(&data_dir).await?;
    heartbeat(&data_dir).await?;
    let metrics = metrics::Metrics::open(&data_dir, "cleaning").await?;
    let heartbeat_dir = data_dir.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            if let Err(error) = heartbeat(&heartbeat_dir).await {
                eprintln!("ohara-cleaning: heartbeat failed: {error}");
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
                    eprintln!("ohara-cleaning: shutdown requested; stopping new work");
                    eprintln!("ohara-cleaning: drain complete");
                }
                return Ok(());
            }
        }
        process_pending(&data_dir, &metrics, shutdown_rx.clone()).await?;
        if *shutdown_rx.borrow() {
            eprintln!("ohara-cleaning: drain complete");
            return Ok(());
        }
    }
}

async fn process_pending(
    data_dir: &Path,
    metrics: &metrics::Metrics,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), CleaningError> {
    let mut entries = tokio::fs::read_dir(data_dir.join("inbox/cleaning")).await?;
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
        if entry.file_type().await?.is_file()
            && entry.path().extension().is_some_and(|ext| ext == "json")
        {
            let path = entry.path();
            let started = Instant::now();
            let result = process_one(data_dir, &path).await;
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
                eprintln!("ohara-cleaning: metrics write failed: {error}");
            }
            if let Err(error) = &result {
                eprintln!("ohara-cleaning: {}: {error}", path.display());
                if let Some(document_id) = path.file_stem().and_then(|value| value.to_str()) {
                    update_catalog(data_dir, document_id, "FAILED", Some(&error.to_string()), 0)
                        .await?;
                }
                dead_letter(data_dir, &path).await?;
                continue;
            }
            tokio::fs::remove_file(path).await?;
        }
    }
}

async fn process_one(data_dir: &Path, queue_path: &Path) -> Result<(), CleaningError> {
    let raw: RawArtifact = decode(&tokio::fs::read(queue_path).await?)?;
    validate_version(raw.schema_version)?;
    let html = tokio::fs::read_to_string(data_dir.join(&raw.raw_path)).await?;
    let article = extract(&html, &raw.source_url)?;
    let markdown = sanitize(&article.content);
    let words = markdown.split_whitespace().count();
    if words < MIN_WORDS {
        return Err(CleaningError::Artifact(format!(
            "document has {words} words; minimum is {MIN_WORDS}"
        )));
    }
    let language =
        whatlang::detect(&markdown).map(|info| format!("{:?}", info.lang()).to_ascii_lowercase());
    let artifact = CleanArtifact {
        schema_version: ARTIFACT_VERSION,
        document_id: raw.document_id.clone(),
        source_url: raw.source_url,
        title: if article.title.trim().is_empty() {
            raw.title
        } else {
            article.title
        },
        markdown,
        language,
        cleaned_at: timestamp(),
    };
    atomic_json(
        &data_dir
            .join("clean")
            .join(format!("{}.json", raw.document_id)),
        &artifact,
    )
    .await?;
    atomic_json(
        &data_dir
            .join("inbox/indexer")
            .join(format!("{}.json", raw.document_id)),
        &artifact,
    )
    .await?;
    update_catalog(data_dir, &raw.document_id, "CLEANED", None, 0).await
}

fn validate_version(version: u8) -> Result<(), CleaningError> {
    (version == ARTIFACT_VERSION).then_some(()).ok_or_else(|| {
        CleaningError::Artifact(format!(
            "unsupported raw artifact schema version {version}; expected {ARTIFACT_VERSION}"
        ))
    })
}

struct Article {
    title: String,
    content: String,
}

fn extract(html: &str, source_url: &str) -> Result<Article, CleaningError> {
    let url =
        url::Url::parse(source_url).map_err(|error| CleaningError::Artifact(error.to_string()))?;
    let mut cursor = std::io::Cursor::new(html.as_bytes());
    let product = readability::extractor::extract(&mut cursor, &url).map_err(|error| {
        CleaningError::Artifact(format!("readability extraction failed: {error}"))
    })?;
    Ok(Article {
        title: product.title,
        content: html2md::parse_html(&product.content),
    })
}

fn sanitize(markdown: &str) -> String {
    let mut output = String::new();
    let mut blank = false;
    for line in markdown.lines() {
        let line = line.trim();
        if line.is_empty() {
            if !blank {
                output.push('\n');
            }
            blank = true;
            continue;
        }
        blank = false;
        if line.to_ascii_lowercase().contains("data:") {
            continue;
        }
        output.push_str(line);
        output.push('\n');
    }
    output.trim().to_string()
}

async fn update_catalog(
    data_dir: &Path,
    document_id: &str,
    status: &str,
    error: Option<&str>,
    chunks: usize,
) -> Result<(), CleaningError> {
    let path = data_dir.join("catalog").join(format!("{document_id}.json"));
    let mut value = if tokio::fs::try_exists(&path).await? {
        decode_value(&tokio::fs::read(&path).await?)?
    } else {
        serde_json::json!({ "id": document_id })
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

async fn dead_letter(data_dir: &Path, path: &Path) -> Result<(), std::io::Error> {
    let filename = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("inbox path has no filename"))?;
    tokio::fs::rename(path, data_dir.join("dead-letter/cleaning").join(filename)).await
}

async fn heartbeat(data_dir: &Path) -> Result<(), std::io::Error> {
    atomic_write(
        &data_dir.join("state/cleaning.heartbeat"),
        timestamp().as_bytes(),
    )
    .await
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut interrupt =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()) {
                Ok(signal) => signal,
                Err(error) => {
                    eprintln!("ohara-cleaning: failed to install SIGINT handler: {error}");
                    if let Err(error) = tokio::signal::ctrl_c().await {
                        eprintln!("ohara-cleaning: shutdown signal failed: {error}");
                    }
                    return;
                }
            };
        let mut terminate =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(error) => {
                    eprintln!("ohara-cleaning: failed to install SIGTERM handler: {error}");
                    if let Err(error) = tokio::signal::ctrl_c().await {
                        eprintln!("ohara-cleaning: shutdown signal failed: {error}");
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
        eprintln!("ohara-cleaning: shutdown signal failed: {error}");
    }
}

fn decode<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, CleaningError> {
    serde_json::from_slice(bytes).map_err(|error| CleaningError::Artifact(error.to_string()))
}

fn decode_value(bytes: &[u8]) -> Result<serde_json::Value, CleaningError> {
    decode(bytes)
}

async fn atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<(), CleaningError> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| CleaningError::Artifact(error.to_string()))?;
    atomic_write(path, &bytes)
        .await
        .map_err(CleaningError::Storage)
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
    use super::{ARTIFACT_VERSION, CleaningError, RawArtifact, validate_version};

    #[test]
    fn raw_v1_fixture_matches_the_input_contract() {
        let artifact = serde_json::from_str::<RawArtifact>(include_str!(
            "../../docs/architecture/fixtures/raw-v1.json"
        ));
        assert!(artifact.is_ok());
    }

    #[test]
    fn rejects_unknown_raw_artifact_versions() {
        assert!(matches!(
            validate_version(ARTIFACT_VERSION + 1),
            Err(CleaningError::Artifact(message)) if message.contains("schema version")
        ));
    }
}
