//! Operator services that coordinate the planes without exposing their stores.
//!
//! The backup path is deliberately here rather than in `control` or
//! `knowledge`: it coordinates the runtime lock and artifact snapshot, while
//! SQLite snapshot semantics remain owned by `control`.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::Serialize;

use crate::config::Config;
use crate::control;

const BACKUP_FORMAT_VERSION: u32 = 1;
const RUNTIME_LOCK_NAME: &str = ".ohara.lock";

/// Operator backup failures.
#[derive(Debug, thiserror::Error)]
pub enum OpsError {
    /// The worker or another operator process currently owns the runtime lock.
    #[error("ohara is busy; runtime lock is held at {path:?}")]
    RuntimeBusy {
        /// Lock path.
        path: PathBuf,
    },
    /// An I/O operation failed at a specific path.
    #[error("I/O at {path:?}: {source}")]
    Io {
        /// Path involved in the operation.
        path: PathBuf,
        /// Underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// SQLite could not produce the consistent control-plane snapshot.
    #[error("control snapshot: {0}")]
    Control(#[from] control::DbError),
    /// The destination already exists; backups never overwrite an existing snapshot.
    #[error("backup destination already exists: {path:?}")]
    DestinationExists {
        /// Existing destination.
        path: PathBuf,
    },
    /// A backup cannot be placed inside the live data root or at the live DB path.
    #[error("backup destination must be outside the live data root: {path:?}")]
    DestinationInsideSource {
        /// Invalid destination.
        path: PathBuf,
    },
    /// A source entry is not a regular file or directory.
    #[error("unsupported runtime entry at {path:?}")]
    UnsupportedEntry {
        /// Unsupported source path.
        path: PathBuf,
    },
    /// The manifest could not be encoded.
    #[error("backup manifest: {0}")]
    Manifest(#[from] serde_json::Error),
}

/// A completed backup location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupReport {
    destination: PathBuf,
}

impl BackupReport {
    /// The committed backup directory.
    #[must_use]
    pub fn destination(&self) -> &Path {
        &self.destination
    }
}

/// Process-wide runtime lock shared by the worker and operator mutations.
///
/// The OS file lock is released automatically when the process exits, including
/// abnormal termination, so a stale lock file does not strand the installation.
pub(crate) struct RuntimeLock {
    file: File,
}

impl RuntimeLock {
    /// Acquires the runtime lock below `data_dir`.
    pub(crate) fn acquire(data_dir: &Path) -> Result<Self, OpsError> {
        fs::create_dir_all(data_dir).map_err(|source| io_error(data_dir, source))?;
        let path = data_dir.join(RUNTIME_LOCK_NAME);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| io_error(&path, source))?;
        if let Err(source) = file.try_lock_exclusive() {
            if source.kind() == std::io::ErrorKind::WouldBlock {
                return Err(OpsError::RuntimeBusy { path });
            }
            return Err(io_error(&path, source));
        }
        file.set_len(0).map_err(|source| io_error(&path, source))?;
        write_process_id(&mut file, &path)?;
        Ok(Self { file })
    }
}

impl Drop for RuntimeLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Creates a consistent, non-overwriting snapshot of the control database and
/// runtime data directory.
///
/// The worker and all operator mutations use the same runtime lock. The
/// snapshot is assembled in a sibling staging directory and renamed into place
/// only after SQLite and every runtime artifact have been copied successfully.
///
/// # Errors
/// [`OpsError::RuntimeBusy`] if the worker is active; [`OpsError::DestinationExists`]
/// or [`OpsError::DestinationInsideSource`] for unsafe destinations; and
/// [`OpsError::Control`], [`OpsError::Io`], or [`OpsError::Manifest`] when the
/// snapshot cannot be completed.
pub fn backup(config: &Config, destination: &Path) -> Result<BackupReport, OpsError> {
    let _runtime_lock = RuntimeLock::acquire(config.data_dir())?;
    let source = absolute_path(config.data_dir())?;
    let destination = absolute_path(destination)?;
    if destination == source
        || destination.starts_with(&source)
        || destination == absolute_path(config.db_path())?
    {
        return Err(OpsError::DestinationInsideSource { path: destination });
    }
    if destination.exists() {
        return Err(OpsError::DestinationExists { path: destination });
    }
    let parent = destination.parent().ok_or_else(|| OpsError::Io {
        path: destination.clone(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "backup destination has no parent",
        ),
    })?;
    fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;

    let name = destination
        .file_name()
        .ok_or_else(|| OpsError::Io {
            path: destination.clone(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "backup destination must name a directory",
            ),
        })?
        .to_string_lossy();
    let staging = parent.join(format!(".{name}.partial-{}", std::process::id()));
    if staging.exists() {
        return Err(OpsError::DestinationExists { path: staging });
    }
    fs::create_dir(&staging).map_err(|source| io_error(&staging, source))?;

    let result = build_backup(config, &source, &staging);
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    fs::rename(&staging, &destination).map_err(|source| io_error(&destination, source))?;
    Ok(BackupReport { destination })
}

#[derive(Serialize)]
struct BackupManifest {
    format_version: u32,
    control_store: &'static str,
    data_root: &'static str,
}

fn build_backup(config: &Config, source: &Path, staging: &Path) -> Result<(), OpsError> {
    let data_destination = staging.join("data");
    fs::create_dir(&data_destination).map_err(|source| io_error(&data_destination, source))?;
    let control_path = absolute_path(config.db_path())?;
    let excluded = [
        control_path.clone(),
        PathBuf::from(format!("{}-wal", control_path.display())),
        PathBuf::from(format!("{}-shm", control_path.display())),
        source.join(RUNTIME_LOCK_NAME),
    ];
    for entry in fs::read_dir(source).map_err(|error| io_error(source, error))? {
        let entry = entry.map_err(|error| io_error(source, error))?;
        let entry_path = entry.path();
        if excluded.iter().any(|path| path == &entry_path) {
            continue;
        }
        copy_entry(
            &entry_path,
            &data_destination.join(entry.file_name()),
            &excluded,
        )?;
    }

    let db = control::connect(config.db_path())?;
    control::backup_to(&db, &staging.join("ohara.db"))?;
    let manifest = serde_json::to_vec_pretty(&BackupManifest {
        format_version: BACKUP_FORMAT_VERSION,
        control_store: "ohara.db",
        data_root: "data",
    })?;
    let manifest_path = staging.join("manifest.json");
    fs::write(&manifest_path, manifest).map_err(|source| io_error(&manifest_path, source))?;
    Ok(())
}

fn copy_entry(source: &Path, destination: &Path, excluded: &[PathBuf]) -> Result<(), OpsError> {
    let metadata = fs::symlink_metadata(source).map_err(|error| io_error(source, error))?;
    if excluded.iter().any(|path| path == source) {
        return Ok(());
    }
    if metadata.is_dir() {
        fs::create_dir(destination).map_err(|error| io_error(destination, error))?;
        for entry in fs::read_dir(source).map_err(|error| io_error(source, error))? {
            let entry = entry.map_err(|error| io_error(source, error))?;
            copy_entry(
                &entry.path(),
                &destination.join(entry.file_name()),
                excluded,
            )?;
        }
    } else if metadata.is_file() {
        fs::copy(source, destination).map_err(|error| io_error(destination, error))?;
    } else {
        return Err(OpsError::UnsupportedEntry {
            path: source.to_path_buf(),
        });
    }
    Ok(())
}

fn write_process_id(file: &mut File, path: &Path) -> Result<(), OpsError> {
    let pid = std::process::id().to_string();
    file.write_all(pid.as_bytes())
        .map_err(|source| io_error(path, source))?;
    file.sync_data().map_err(|source| io_error(path, source))
}

fn absolute_path(path: &Path) -> Result<PathBuf, OpsError> {
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|current| current.join(path))
            .map_err(|source| io_error(path, source))?
    };
    let mut existing = candidate.clone();
    let mut missing = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name() else {
            return Ok(candidate);
        };
        missing.push(name.to_os_string());
        existing.pop();
    }
    let mut resolved = fs::canonicalize(&existing).map_err(|source| io_error(&existing, source))?;
    for name in missing.iter().rev() {
        resolved.push(name);
    }
    Ok(resolved)
}

fn io_error(path: &Path, source: std::io::Error) -> OpsError {
    OpsError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use std::fs;

    use super::{OpsError, RuntimeLock, backup};
    use crate::config::Config;

    #[test]
    fn runtime_lock_rejects_a_second_owner_and_releases_on_drop() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let first = RuntimeLock::acquire(dir.path()).expect("first lock");
        assert!(matches!(
            RuntimeLock::acquire(dir.path()),
            Err(OpsError::RuntimeBusy { .. })
        ));
        drop(first);
        RuntimeLock::acquire(dir.path()).expect("lock after release");
    }

    #[test]
    fn backup_is_staged_and_copies_data_without_overwriting() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let data = dir.path().join("data");
        let destination = dir.path().join("backup");
        fs::create_dir_all(data.join("raw")).expect("raw directory");
        fs::write(data.join("raw/article.html.gz"), b"payload").expect("raw payload");
        let config_path = dir.path().join("ohara.toml");
        fs::write(&config_path, format!("data_dir = {data:?}\n")).expect("config file");
        let config = Config::load(Some(&config_path)).expect("valid config");

        let report = backup(&config, &destination).expect("backup");
        assert_eq!(
            report.destination().canonicalize().unwrap(),
            destination.canonicalize().unwrap()
        );
        assert_eq!(
            fs::read(destination.join("data/raw/article.html.gz")).expect("copied payload"),
            b"payload"
        );
        assert!(destination.join("ohara.db").is_file());
        assert!(destination.join("manifest.json").is_file());
        assert!(matches!(
            backup(&config, &destination),
            Err(OpsError::DestinationExists { .. })
        ));
        assert!(matches!(
            backup(&config, &data.join("nested-backup")),
            Err(OpsError::DestinationInsideSource { .. })
        ));
    }
}
