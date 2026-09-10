//! Operator services that coordinate the planes without exposing their stores.
//!
//! The backup path is deliberately here rather than in `control` or
//! `knowledge`: it coordinates the runtime lock and artifact snapshot, while
//! `SQLite` snapshot semantics remain owned by `control`.

mod entity_merge;
mod metrics;
mod prune;

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::Serialize;

use crate::config::Config;
use crate::control;

const BACKUP_FORMAT_VERSION: u32 = 1;
const RUNTIME_LOCK_NAME: &str = ".ohara.lock";

#[cfg(all(feature = "ladybug", test))]
pub(super) use entity_merge::execute_entity_merges;
pub use entity_merge::merge_entities;
pub use metrics::{MetricsReport, metrics};
pub use prune::{PruneReport, prune};

/// Operator failures.
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
    /// `SQLite` could not produce the consistent control-plane snapshot.
    #[error("control snapshot: {0}")]
    Control(#[from] control::DbError),
    /// The knowledge plane could not complete an operator mutation.
    #[error("knowledge store: {0}")]
    Knowledge(#[from] crate::knowledge::KnowledgeError),
    /// Entity merge requires the embedded `LadybugDB` implementation.
    #[error("entity merge requires the `ladybug` feature")]
    KnowledgeFeatureDisabled,
    /// The requested document is not registered in the control plane.
    #[error("document not found: {doc_id}")]
    DocumentNotFound {
        /// Requested document id.
        doc_id: String,
    },
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
    /// A registered raw payload points outside the configured `data/raw` root.
    #[error("raw payload path {path:?} is outside the raw root {root:?}")]
    RawPathOutsideRoot {
        /// Unsafe stored path.
        path: PathBuf,
        /// Allowed raw payload root.
        root: PathBuf,
    },
    /// A registered raw payload is not a regular file below the raw root.
    #[error("raw payload path is not a regular file below {root:?}: {path:?}")]
    RawPathUnsafe {
        /// Stored payload path.
        path: PathBuf,
        /// Allowed raw payload root.
        root: PathBuf,
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

/// Result of resetting a document's failed or interrupted jobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequeueReport {
    /// Document whose jobs were reset.
    pub doc_id: String,
    /// Number of `DEAD` or `RUNNING` jobs reset to `PENDING`.
    pub jobs_reset: usize,
}

/// Result of marking a document archived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveReport {
    /// Document marked `ARCHIVED`.
    pub doc_id: String,
}

/// Result of recording a deletion intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteReport {
    /// Document whose deletion was requested.
    pub doc_id: String,
}

/// Summary of one offline entity-resolution merge run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EntityMergeReport {
    /// Pending review rows examined.
    pub reviews_examined: usize,
    /// New `SQLite` merge audit rows recorded.
    pub merges_recorded: usize,
    /// Knowledge-plane folds replayed, including repair folds.
    pub folds_replayed: usize,
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
/// only after `SQLite` and every runtime artifact have been copied successfully.
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

/// Resets a document's failed or interrupted jobs to a fresh retry budget.
/// Completed stages remain complete; the worker resumes at the first unfinished
/// stage. The runtime lock prevents a worker claim from racing the reset.
///
/// # Errors
/// [`OpsError::DocumentNotFound`] if `doc_id` is unknown, [`OpsError::RuntimeBusy`]
/// if the worker is active, or a control-plane error.
pub fn requeue(config: &Config, doc_id: &str) -> Result<RequeueReport, OpsError> {
    let (_runtime_lock, db) = open_control(config)?;
    require_document(&db, doc_id)?;
    let jobs_reset = control::requeue(&db, doc_id, &control::now())?;
    Ok(RequeueReport {
        doc_id: doc_id.to_string(),
        jobs_reset,
    })
}

/// Archives a document while retaining its chunks for query results.
/// Archived documents cannot be claimed or scheduled for re-crawl.
///
/// # Errors
/// [`OpsError::DocumentNotFound`] if `doc_id` is unknown, [`OpsError::RuntimeBusy`]
/// if the worker is active, or a control-plane error.
pub fn archive(config: &Config, doc_id: &str) -> Result<ArchiveReport, OpsError> {
    let (_runtime_lock, db) = open_control(config)?;
    require_document(&db, doc_id)?;
    control::archive(&db, doc_id, &control::now())?;
    Ok(ArchiveReport {
        doc_id: doc_id.to_string(),
    })
}

/// Records a knowledge-first deletion intent for a document.
///
/// The command is intentionally asynchronous: the worker or next boot removes
/// the document from every knowledge collection before the `SQLite` cascade runs.
/// Repeating the command is idempotent.
///
/// # Errors
/// [`OpsError::DocumentNotFound`] if `doc_id` is unknown, [`OpsError::RuntimeBusy`]
/// if the worker is active, or a control-plane error.
pub fn delete_document(config: &Config, doc_id: &str) -> Result<DeleteReport, OpsError> {
    let (_runtime_lock, db) = open_control(config)?;
    require_document(&db, doc_id)?;
    control::request_deletion(&db, doc_id, Some(OPERATOR_DELETE_REASON))?;
    Ok(DeleteReport {
        doc_id: doc_id.to_string(),
    })
}

const OPERATOR_DELETE_REASON: &str = "operator request";

fn open_control(config: &Config) -> Result<(RuntimeLock, control::ControlDb), OpsError> {
    let runtime_lock = RuntimeLock::acquire(config.data_dir())?;
    let db = control::connect(config.db_path())?;
    Ok((runtime_lock, db))
}

fn require_document(db: &control::ControlDb, doc_id: &str) -> Result<control::Document, OpsError> {
    control::get(db, doc_id)?.ok_or_else(|| OpsError::DocumentNotFound {
        doc_id: doc_id.to_string(),
    })
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
    use std::path::Path;

    #[cfg(feature = "ladybug")]
    use super::execute_entity_merges;
    use super::{OpsError, RuntimeLock, archive, backup, delete_document, prune, requeue};
    use crate::config::Config;
    use crate::control::{self, NewDocument, Stage};
    #[cfg(feature = "ladybug")]
    use crate::knowledge::{EntityRecord, EntityType, KnowledgeStore, LadybugStore};

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

    #[test]
    fn lifecycle_commands_use_the_control_facade_and_are_idempotent() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let data = dir.path().join("data");
        let config_path = dir.path().join("ohara.toml");
        fs::create_dir_all(&data).expect("data directory");
        fs::write(&config_path, format!("data_dir = {data:?}\n")).expect("config file");
        let config = Config::load(Some(&config_path)).expect("valid config");

        let db = control::connect(config.db_path()).expect("control store");
        let doc_id = control::insert_new(
            &db,
            config.data_dir(),
            &NewDocument {
                source_url: "https://example.com/lifecycle".to_string(),
                source_url_normalized: "https://example.com/lifecycle".to_string(),
                priority: 5,
                pipeline_version: "test".to_string(),
            },
            "2026-09-06 12:00:00",
        )
        .expect("document")
        .doc_id()
        .to_string();
        let job = control::claim_next(&db, Stage::Scrape, "test", "2026-09-06 12:00:00", 60)
            .expect("claim")
            .expect("scrape job");
        control::dead(
            &db,
            job.job_id(),
            &doc_id,
            "test failure",
            "2026-09-06 12:00:00",
        )
        .expect("dead job");
        drop(db);

        let requeued = requeue(&config, &doc_id).expect("requeue");
        assert_eq!(requeued.jobs_reset, 1);
        assert_eq!(
            requeue(&config, &doc_id)
                .expect("idempotent requeue")
                .jobs_reset,
            0
        );

        let archived = archive(&config, &doc_id).expect("archive");
        assert_eq!(archived.doc_id, doc_id);
        let db = control::connect(config.db_path()).expect("control store");
        assert_eq!(
            control::get(&db, &doc_id)
                .expect("document lookup")
                .unwrap()
                .status,
            control::DocStatus::Archived
        );
        assert!(
            control::claim_next(&db, Stage::Scrape, "test", "2026-09-06 12:00:00", 60)
                .expect("archived jobs are not claimable")
                .is_none()
        );
        drop(db);

        delete_document(&config, &doc_id).expect("delete intent");
        delete_document(&config, &doc_id).expect("idempotent delete intent");
        let db = control::connect(config.db_path()).expect("control store");
        assert_eq!(
            control::pending_deletions(&db)
                .expect("pending intents")
                .len(),
            1
        );
    }

    #[test]
    fn prune_is_dry_run_safe_idempotent_and_skips_live_jobs() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let data = dir.path().join("data");
        let config_path = dir.path().join("ohara.toml");
        fs::create_dir_all(&data).expect("data directory");
        fs::write(
            &config_path,
            format!("data_dir = {data:?}\n[retention]\nraw_max_bytes = 3\n"),
        )
        .expect("config file");
        let config = Config::load(Some(&config_path)).expect("valid config");
        let db = control::connect(config.db_path()).expect("control store");

        let ready_a = control::insert_new(
            &db,
            config.data_dir(),
            &NewDocument {
                source_url: "https://example.com/ready-a".to_string(),
                source_url_normalized: "https://example.com/ready-a".to_string(),
                priority: 5,
                pipeline_version: "test".to_string(),
            },
            "2026-09-06 12:00:00",
        )
        .expect("ready a")
        .doc_id()
        .to_string();
        let ready_b = control::insert_new(
            &db,
            config.data_dir(),
            &NewDocument {
                source_url: "https://example.com/ready-b".to_string(),
                source_url_normalized: "https://example.com/ready-b".to_string(),
                priority: 5,
                pipeline_version: "test".to_string(),
            },
            "2026-09-06 12:00:00",
        )
        .expect("ready b")
        .doc_id()
        .to_string();
        let live = control::insert_new(
            &db,
            config.data_dir(),
            &NewDocument {
                source_url: "https://example.com/live".to_string(),
                source_url_normalized: "https://example.com/live".to_string(),
                priority: 5,
                pipeline_version: "test".to_string(),
            },
            "2026-09-06 12:00:00",
        )
        .expect("live")
        .doc_id()
        .to_string();
        for doc_id in [&ready_a, &ready_b, &live] {
            db.raw()
                .execute(
                    "UPDATE documents SET status = 'CLEANED' WHERE doc_id = ?1",
                    [doc_id],
                )
                .expect("mark clean");
        }
        for doc_id in [&ready_a, &ready_b] {
            db.raw()
                .execute(
                    "UPDATE jobs SET status = 'DONE' WHERE doc_id = ?1",
                    [doc_id],
                )
                .expect("finish scrape");
        }
        let paths = [&ready_a, &ready_b, &live].map(|doc_id| {
            control::get(&db, doc_id)
                .expect("lookup")
                .expect("document")
                .raw_file_path
        });
        drop(db);
        for path in &paths {
            fs::create_dir_all(Path::new(path).parent().expect("raw parent"))
                .expect("raw directory");
            fs::write(path, b"abc").expect("raw payload");
        }

        let dry_run = prune(&config, true).expect("dry run");
        assert!(dry_run.dry_run);
        assert_eq!(dry_run.selected, 1);
        assert_eq!(dry_run.deleted, 0);
        assert_eq!(dry_run.skipped_active, 1);
        assert!(paths.iter().all(|path| Path::new(path).is_file()));

        let run = prune(&config, false).expect("prune");
        assert_eq!(run.deleted, 1);
        assert_eq!(run.reclaimed_bytes, 3);
        assert!(
            paths
                .iter()
                .filter(|path| Path::new(path).is_file())
                .count()
                >= 2
        );

        let repeat = prune(&config, false).expect("repeat prune");
        assert_eq!(repeat.deleted, 0);
        assert_eq!(repeat.missing, 1);
    }

    #[test]
    fn prune_rejects_a_registered_raw_path_outside_the_raw_root() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let data = dir.path().join("data");
        let config_path = dir.path().join("ohara.toml");
        fs::create_dir_all(&data).expect("data directory");
        fs::write(
            &config_path,
            format!("data_dir = {data:?}\n[retention]\nraw_max_bytes = 1\n"),
        )
        .expect("config file");
        let config = Config::load(Some(&config_path)).expect("valid config");
        let db = control::connect(config.db_path()).expect("control store");
        let doc_id = control::testing::seed_doc(&db, "unsafe");
        db.raw()
            .execute(
                "UPDATE documents SET status = 'CLEANED', raw_file_path = ?2 WHERE doc_id = ?1",
                rusqlite::params![doc_id, dir.path().join("outside.html.gz").to_string_lossy()],
            )
            .expect("unsafe path");
        db.raw()
            .execute(
                "UPDATE jobs SET status = 'DONE' WHERE doc_id = ?1",
                [&doc_id],
            )
            .expect("finish scrape");
        drop(db);

        assert!(matches!(
            prune(&config, false),
            Err(OpsError::RawPathOutsideRoot { .. })
        ));
    }

    #[cfg(feature = "ladybug")]
    #[test]
    fn entity_merge_uses_mentions_for_winner_and_repairs_the_graph() {
        let db = control::testing::boot();
        control::ensure_entity(&db, "loser", "Ada", "PERSON", None).expect("loser");
        control::ensure_entity(&db, "winner", "Ada Lovelace", "PERSON", None).expect("winner");
        control::upsert_alias(&db, "ada", "PERSON", "loser").expect("alias");
        control::er_review_candidate(&db, "loser", "winner", 0.91).expect("review");

        let store = LadybugStore::in_memory(4).expect("knowledge store");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            for (id, name) in [("loser", "Ada"), ("winner", "Ada Lovelace")] {
                store
                    .upsert_entity(&EntityRecord {
                        entity_id: id.to_string(),
                        canonical_name: name.to_string(),
                        entity_type: EntityType::Person,
                        subtype: None,
                    })
                    .await
                    .expect("entity node");
            }
            store
                .link_mention("chunk-loser", "loser")
                .await
                .expect("mention");
            store
                .link_mention("chunk-winner-1", "winner")
                .await
                .expect("mention");
            store
                .link_mention("chunk-winner-2", "winner")
                .await
                .expect("mention");

            let report = execute_entity_merges(&db, &store).await.expect("merge");
            assert_eq!(report.reviews_examined, 1);
            assert_eq!(report.merges_recorded, 1);
            assert_eq!(
                control::resolve_entity(&db, "loser").expect("resolved loser"),
                "winner"
            );
            assert_eq!(
                store
                    .chunks_for_entities(&["winner"])
                    .await
                    .expect("winner mentions")
                    .len(),
                3
            );
            assert!(
                store
                    .chunks_for_entities(&["loser"])
                    .await
                    .expect("loser mentions")
                    .is_empty()
            );
        });
    }
}
