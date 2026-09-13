//! Durable counters and latency snapshots for the retrieval process.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Snapshot {
    pub(crate) process: String,
    pub(crate) input_count: u64,
    pub(crate) output_count: u64,
    pub(crate) failure_count: u64,
    pub(crate) total_latency_ms: u64,
    pub(crate) last_latency_ms: u64,
    pub(crate) max_latency_ms: u64,
    pub(crate) updated_at: String,
}

impl Snapshot {
    fn new(process: &str) -> Self {
        Self {
            process: process.to_string(),
            input_count: 0,
            output_count: 0,
            failure_count: 0,
            total_latency_ms: 0,
            last_latency_ms: 0,
            max_latency_ms: 0,
            updated_at: timestamp(),
        }
    }
}

pub(crate) struct Metrics {
    path: PathBuf,
    snapshot: Mutex<Snapshot>,
}

impl Metrics {
    pub(crate) async fn open(data_dir: &Path, process: &str) -> Result<Self, std::io::Error> {
        let path = metric_path(data_dir, process);
        let snapshot = load(&path, process).await?;
        let metrics = Self {
            path,
            snapshot: Mutex::new(snapshot),
        };
        metrics.persist().await?;
        Ok(metrics)
    }

    pub(crate) async fn record(
        &self,
        input_count: u64,
        output_count: u64,
        failure_count: u64,
        latency: Duration,
    ) -> Result<(), std::io::Error> {
        let mut snapshot = self.snapshot.lock().await;
        snapshot.input_count = snapshot.input_count.saturating_add(input_count);
        snapshot.output_count = snapshot.output_count.saturating_add(output_count);
        snapshot.failure_count = snapshot.failure_count.saturating_add(failure_count);
        let latency_ms = u64::try_from(latency.as_millis()).unwrap_or(u64::MAX);
        snapshot.total_latency_ms = snapshot.total_latency_ms.saturating_add(latency_ms);
        snapshot.last_latency_ms = latency_ms;
        snapshot.max_latency_ms = snapshot.max_latency_ms.max(latency_ms);
        snapshot.updated_at = timestamp();
        self.persist_snapshot(&snapshot).await
    }

    pub(crate) async fn snapshot(&self) -> Snapshot {
        self.snapshot.lock().await.clone()
    }

    async fn persist(&self) -> Result<(), std::io::Error> {
        let snapshot = self.snapshot.lock().await;
        self.persist_snapshot(&snapshot).await
    }

    async fn persist_snapshot(&self, snapshot: &Snapshot) -> Result<(), std::io::Error> {
        let bytes = serde_json::to_vec_pretty(snapshot).map_err(std::io::Error::other)?;
        atomic_write(&self.path, &bytes).await
    }
}

pub(crate) async fn read_or_default(
    data_dir: &Path,
    process: &str,
) -> Result<Snapshot, std::io::Error> {
    load(&metric_path(data_dir, process), process).await
}

async fn load(path: &Path, process: &str) -> Result<Snapshot, std::io::Error> {
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(serde_json::from_slice::<Snapshot>(&bytes)
            .ok()
            .filter(|snapshot| snapshot.process == process)
            .unwrap_or_else(|| Snapshot::new(process))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Snapshot::new(process)),
        Err(error) => Err(error),
    }
}

fn metric_path(data_dir: &Path, process: &str) -> PathBuf {
    data_dir
        .join("state")
        .join(format!("{process}.metrics.json"))
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
