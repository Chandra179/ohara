//! Durable fixture artifact helpers.

use crate::{Error, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Write bytes with a temporary sibling and an atomic rename.
pub(crate) fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|source| Error::io(format!("create {}", parent.display()), source))?;
    }
    let temporary = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    std::fs::write(&temporary, content)
        .map_err(|source| Error::io(format!("write {}", temporary.display()), source))?;
    std::fs::rename(&temporary, path)
        .map_err(|source| Error::io(format!("rename {}", path.display()), source))
}

/// Serialize JSON with stable human-readable indentation and atomically write it.
pub(crate) fn write_json(path: &Path, value: &Value) -> Result<()> {
    let body = serde_json::to_vec_pretty(value).map_err(|source| Error::Json {
        context: path.display().to_string(),
        source,
    })?;
    atomic_write(path, &body)
}

/// Return regular JSON files directly inside a directory.
pub(crate) fn json_files(path: &Path) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(Error::io(format!("read {}", path.display()), source)),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| Error::io("read directory entry", source))?;
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
            && path.is_file()
        {
            files.push(path);
        }
    }
    Ok(files)
}

/// Read and decode a JSON file.
pub(crate) fn read_json(path: &Path) -> Result<Value> {
    let bytes = std::fs::read(path)
        .map_err(|source| Error::io(format!("read {}", path.display()), source))?;
    serde_json::from_slice(&bytes).map_err(|source| Error::Json {
        context: path.display().to_string(),
        source,
    })
}

/// Create the standard isolated data-directory layout.
pub(crate) fn seed_directories(data_dir: &Path) -> Result<()> {
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
        std::fs::create_dir_all(data_dir.join(relative)).map_err(|source| {
            Error::io(
                format!("create {}", data_dir.join(relative).display()),
                source,
            )
        })?;
    }
    Ok(())
}

/// Create a uniquely named temporary directory for one isolated tool run.
pub(crate) fn temporary_directory(prefix: &str) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir(&path)
        .map_err(|source| Error::io(format!("create {}", path.display()), source))?;
    Ok(path)
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}
