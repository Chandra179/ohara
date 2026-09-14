//! Shared infrastructure for Ohara's deterministic verification tools.

mod artifacts;
mod http;
pub mod latency;
mod metrics;
pub mod pipeline_fixture;
mod process;
mod providers;
pub mod qdrant;
pub mod quality;
pub mod resource;
mod workload;

mod error;

pub use error::{Error, Result};

/// Return the repository root containing the workspace `Cargo.toml`.
pub(crate) fn repository_root() -> Result<std::path::PathBuf> {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .map(std::path::Path::to_path_buf)
        .ok_or_else(|| Error::Message("tools manifest has no repository parent".to_owned()))
}

/// Read a positive integer environment setting.
pub(crate) fn positive_int(name: &str, default: usize) -> Result<usize> {
    let value = std::env::var(name).unwrap_or_else(|_| default.to_string());
    let parsed = value
        .parse::<usize>()
        .map_err(|_| Error::Message(format!("{name} must be a positive integer")))?;
    if parsed == 0 {
        return Err(Error::Message(format!("{name} must be at least 1")));
    }
    Ok(parsed)
}

/// Read a positive floating-point environment setting.
pub(crate) fn positive_float(name: &str, default: f64) -> Result<f64> {
    let value = std::env::var(name).unwrap_or_else(|_| default.to_string());
    let parsed = value
        .parse::<f64>()
        .map_err(|_| Error::Message(format!("{name} must be a positive number")))?;
    if !parsed.is_finite() || parsed <= 0.0 {
        return Err(Error::Message(format!("{name} must be greater than zero")));
    }
    Ok(parsed)
}

/// Validate a condition and return a useful harness error when it is false.
pub(crate) fn check(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Error::Message(message.into()))
    }
}
