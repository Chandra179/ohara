use std::path::PathBuf;

/// Errors produced by verification tooling.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An operating-system or filesystem operation failed.
    #[error("{context}: {source}")]
    Io {
        /// Operation being performed when the error occurred.
        context: String,
        /// Underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// An HTTP request failed before receiving a response.
    #[error("HTTP request failed for {url}: {source}")]
    Http {
        /// URL involved in the request.
        url: String,
        /// Underlying HTTP client failure.
        #[source]
        source: reqwest::Error,
    },
    /// A JSON payload could not be decoded.
    #[error("invalid JSON from {context}: {source}")]
    Json {
        /// Endpoint or file containing the JSON.
        context: String,
        /// Underlying JSON decoder failure.
        #[source]
        source: serde_json::Error,
    },
    /// A child process failed to start or returned an invalid result.
    #[error("process {name}: {message}")]
    Process {
        /// Process name.
        name: String,
        /// Failure details.
        message: String,
    },
    /// A tool-level invariant was violated.
    #[error("{0}")]
    Message(String),
    /// A benchmark output path could not be written.
    #[error("could not write benchmark output {path}: {source}")]
    Output {
        /// Output path.
        path: PathBuf,
        /// Underlying output failure.
        #[source]
        source: std::io::Error,
    },
}

impl Error {
    pub(crate) fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

/// Result type used by Ohara verification tools.
pub type Result<T> = std::result::Result<T, Error>;

impl From<tokio::task::JoinError> for Error {
    fn from(source: tokio::task::JoinError) -> Self {
        Self::Message(format!("provider task failed: {source}"))
    }
}
