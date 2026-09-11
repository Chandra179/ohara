//! Read-only operator metrics assembled from the control plane and runtime files.

use std::fs;
use std::path::Path;

use serde::Serialize;

use crate::config::Config;
use crate::control;

use super::OpsError;

/// Operator-facing metrics snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MetricsReport {
    /// Durable `SQLite` metrics, flattened into the operator JSON object.
    #[serde(flatten)]
    pub control: control::MetricsSnapshot,
    /// Number of regular files currently below `data/raw`.
    pub raw_files: u64,
    /// Total bytes currently used by regular files below `data/raw`.
    pub raw_bytes: u64,
    /// Configured raw-payload byte ceiling, if enabled.
    pub raw_max_bytes: Option<u64>,
    /// Configured raw-payload age ceiling, if enabled.
    pub raw_max_age_days: Option<u64>,
}

/// Reads a metrics snapshot while holding the same runtime lock as operator
/// mutations. The command is read-only, but the lock keeps `SQLite` and raw-file
/// observations from racing a worker or prune operation.
///
/// # Errors
/// Returns [`OpsError`] if the runtime is busy, `SQLite` cannot be read, or the
/// raw directory cannot be inspected.
pub fn metrics(config: &Config) -> Result<MetricsReport, OpsError> {
    let (_runtime_lock, db) = super::open_control(config)?;
    metrics_from_db(config, &db)
}

/// Reads metrics without acquiring the exclusive runtime lock.
///
/// This variant is for a live read-only API while the worker is active. `SQLite`
/// WAL provides a consistent database snapshot for the aggregate query; raw
/// file usage is an intentionally approximate observation because files can
/// change while the worker or a retention operation runs.
///
/// # Errors
/// Returns [`OpsError::Control`] if the control store cannot be opened or read,
/// or [`OpsError::Io`] if the raw directory cannot be inspected.
pub fn metrics_read_only(config: &Config) -> Result<MetricsReport, OpsError> {
    let db = control::connect(config.db_path())?;
    metrics_from_db(config, &db)
}

fn metrics_from_db(config: &Config, db: &control::ControlDb) -> Result<MetricsReport, OpsError> {
    let control = control::metrics(db, &control::now())?;
    let (raw_files, raw_bytes) = raw_usage(&config.data_dir().join("raw"))?;
    Ok(MetricsReport {
        control,
        raw_files,
        raw_bytes,
        raw_max_bytes: config.retention().raw_max_bytes(),
        raw_max_age_days: config.retention().raw_max_age_days(),
    })
}

fn raw_usage(root: &Path) -> Result<(u64, u64), OpsError> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok((0, 0)),
        Err(source) => return Err(super::io_error(root, source)),
    };
    let mut files: u64 = 0;
    let mut bytes: u64 = 0;
    for entry in entries {
        let entry = entry.map_err(|source| super::io_error(root, source))?;
        let path = entry.path();
        if !entry
            .file_type()
            .map_err(|source| super::io_error(&path, source))?
            .is_file()
        {
            continue;
        }
        let size = entry
            .metadata()
            .map_err(|source| super::io_error(&path, source))?
            .len();
        files += 1;
        bytes = bytes.saturating_add(size);
    }
    Ok((files, bytes))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::{metrics_read_only, raw_usage};
    use crate::config::Config;
    use crate::ops::RuntimeLock;

    #[test]
    fn raw_usage_counts_regular_files_and_ignores_missing_roots() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let raw = dir.path().join("raw");
        assert_eq!(raw_usage(&raw).expect("missing root is empty"), (0, 0));
        std::fs::create_dir_all(&raw).expect("raw directory");
        std::fs::write(raw.join("one.html.gz"), b"123").expect("payload");
        std::fs::create_dir(raw.join("nested")).expect("nested directory");
        assert_eq!(raw_usage(&raw).expect("raw usage"), (1, 3));
    }

    #[test]
    fn read_only_metrics_does_not_require_the_runtime_lock() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config_path = directory.path().join("ohara.toml");
        std::fs::write(&config_path, format!("data_dir = {:?}\n", directory.path()))
            .expect("config file");
        let config = Config::load(Some(&config_path)).expect("config");
        let _lock = RuntimeLock::acquire(config.data_dir()).expect("runtime lock");

        let report = metrics_read_only(&config).expect("live metrics");
        assert_eq!(report.raw_files, 0);
        assert_eq!(report.raw_bytes, 0);
    }
}
