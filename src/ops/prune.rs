//! Raw-payload retention operator (§7.10).
//!
//! The control plane decides which documents are safe to inspect. This module
//! owns the filesystem seam: policy selection, path containment, file metadata,
//! and idempotent unlinking. The runtime lock is acquired by the parent facade
//! before this module touches either store.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::config::Config;
use crate::control;

use super::{OpsError, absolute_path, io_error, open_control};

const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

/// Summary of one raw-retention run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PruneReport {
    /// Whether the run only planned removals.
    pub dry_run: bool,
    /// Whether at least one retention limit was configured.
    pub policy_active: bool,
    /// Number of control-plane candidates inspected.
    pub examined: usize,
    /// Number of regular raw files eligible for selection.
    pub eligible: usize,
    /// Number of candidates skipped because a job is pending or running.
    pub skipped_active: usize,
    /// Number of eligible document rows whose payload was already absent.
    pub missing: usize,
    /// Number of files selected by the retention policy.
    pub selected: usize,
    /// Bytes selected by the retention policy.
    pub planned_bytes: u64,
    /// Number of files actually unlinked.
    pub deleted: usize,
    /// Bytes actually reclaimed by unlinking files.
    pub reclaimed_bytes: u64,
}

#[derive(Debug)]
struct RawFile {
    doc_id: String,
    path: PathBuf,
    size: u64,
    modified: SystemTime,
}

/// Applies the configured raw-payload retention policy.
///
/// Only documents in a completed or archived milestone are considered. Files
/// are selected oldest-first, with the age threshold applied before the byte
/// budget. The operation never changes `SQLite` rows or knowledge-plane data.
///
/// # Errors
/// Returns [`OpsError::RuntimeBusy`] when the worker owns the runtime lock,
/// [`OpsError::RawPathOutsideRoot`] or [`OpsError::RawPathUnsafe`] for an
/// unsafe registered path, or [`OpsError::Control`] / [`OpsError::Io`] for
/// store and filesystem failures.
pub fn prune(config: &Config, dry_run: bool) -> Result<PruneReport, OpsError> {
    let (_runtime_lock, db) = open_control(config)?;
    let candidates = control::raw_retention_candidates(&db)?;
    let raw_root = absolute_path(&config.data_dir().join("raw"))?;
    let policy = config.retention();
    let policy_active = policy.raw_max_bytes().is_some() || policy.raw_max_age_days().is_some();
    let cutoff = policy.raw_max_age_days().map(age_cutoff).transpose()?;
    let (files, skipped_active, missing) = collect_files(candidates, &raw_root)?;
    let mut report = PruneReport {
        dry_run,
        policy_active,
        examined: files.len() + skipped_active + missing,
        skipped_active,
        missing,
        eligible: files.len(),
        ..PruneReport::default()
    };
    let selected = select_files(&files, cutoff, policy.raw_max_bytes());
    report.selected = selected.iter().filter(|is_selected| **is_selected).count();
    report.planned_bytes = files
        .iter()
        .zip(&selected)
        .filter(|(_, is_selected)| **is_selected)
        .map(|(file, _)| file.size)
        .fold(0_u64, u64::saturating_add);
    if dry_run || !policy_active {
        return Ok(report);
    }

    delete_selected(&mut report, files, selected)?;
    Ok(report)
}

fn collect_files(
    candidates: Vec<control::RawRetentionCandidate>,
    raw_root: &Path,
) -> Result<(Vec<RawFile>, usize, usize), OpsError> {
    let mut files = Vec::new();
    let mut skipped_active = 0;
    let mut missing = 0;
    for candidate in candidates {
        if candidate.has_live_job {
            skipped_active += 1;
            continue;
        }
        let path = safe_raw_path(raw_root, Path::new(&candidate.raw_file_path))?;
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing += 1;
                continue;
            }
            Err(error) => return Err(io_error(&path, error)),
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(OpsError::RawPathUnsafe {
                path,
                root: raw_root.to_path_buf(),
            });
        }
        let canonical = fs::canonicalize(&path).map_err(|error| io_error(&path, error))?;
        if !canonical.starts_with(raw_root) {
            return Err(OpsError::RawPathOutsideRoot {
                path: canonical,
                root: raw_root.to_path_buf(),
            });
        }
        let modified = metadata
            .modified()
            .map_err(|error| io_error(&path, error))?;
        files.push(RawFile {
            doc_id: candidate.doc_id,
            path,
            size: metadata.len(),
            modified,
        });
    }
    files.sort_by(|left, right| {
        left.modified
            .cmp(&right.modified)
            .then_with(|| left.doc_id.cmp(&right.doc_id))
    });
    Ok((files, skipped_active, missing))
}

fn select_files(
    files: &[RawFile],
    cutoff: Option<SystemTime>,
    max_bytes: Option<u64>,
) -> Vec<bool> {
    let mut selected = vec![false; files.len()];
    let mut remaining_bytes = files
        .iter()
        .map(|file| file.size)
        .fold(0_u64, u64::saturating_add);
    if let Some(cutoff) = cutoff {
        for (index, file) in files.iter().enumerate() {
            if file.modified <= cutoff {
                selected[index] = true;
                remaining_bytes = remaining_bytes.saturating_sub(file.size);
            }
        }
    }
    if let Some(max_bytes) = max_bytes {
        for (index, file) in files.iter().enumerate() {
            if remaining_bytes <= max_bytes {
                break;
            }
            if !selected[index] {
                selected[index] = true;
                remaining_bytes = remaining_bytes.saturating_sub(file.size);
            }
        }
    }
    selected
}

fn delete_selected(
    report: &mut PruneReport,
    files: Vec<RawFile>,
    selected: Vec<bool>,
) -> Result<(), OpsError> {
    for (file, is_selected) in files.into_iter().zip(selected) {
        if !is_selected {
            continue;
        }
        match fs::remove_file(&file.path) {
            Ok(()) => {
                report.deleted += 1;
                report.reclaimed_bytes = report.reclaimed_bytes.saturating_add(file.size);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                report.missing += 1;
            }
            Err(error) => return Err(io_error(&file.path, error)),
        }
    }
    Ok(())
}

fn age_cutoff(days: u64) -> Result<SystemTime, OpsError> {
    let seconds = days
        .checked_mul(SECONDS_PER_DAY)
        .ok_or_else(|| OpsError::Io {
            path: PathBuf::from("retention.raw_max_age_days"),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "age exceeds the supported duration",
            ),
        })?;
    SystemTime::now()
        .checked_sub(Duration::from_secs(seconds))
        .ok_or_else(|| OpsError::Io {
            path: PathBuf::from("retention.raw_max_age_days"),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "age is outside the system clock range",
            ),
        })
}

fn safe_raw_path(raw_root: &Path, stored: &Path) -> Result<PathBuf, OpsError> {
    let path = absolute_path(stored)?;
    if !path.starts_with(raw_root) {
        return Err(OpsError::RawPathOutsideRoot {
            path,
            root: raw_root.to_path_buf(),
        });
    }
    Ok(path)
}
