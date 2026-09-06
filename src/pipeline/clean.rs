//! Stage 2 — Clean (§8): boilerplate removal → Markdown → sanitize → hash/dedup →
//! quality + language gate, and the [`Extractor`] port (§1.3). The readability/
//! html2md implementation lands with the §15 step 3 build step.

use crate::Class;
use crate::config::Config;
use crate::control::ClaimedJob;

use super::StageError;

/// Extracted article (§8 Stage 2): boilerplate removed, structure preserved.
#[derive(Debug, Clone)]
pub struct ExtractedArticle {
    /// Page title, if detected.
    pub title: Option<String>,
    /// Byline, if detected.
    pub byline: Option<String>,
    /// Primary content as Markdown (headings, lists, tables preserved).
    pub markdown: String,
}

/// Extraction failures (§9): `Err` is reserved for "the operation couldn't do its
/// job" — quality outcomes are values in the stage signature (§10).
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    /// No primary content could be located in the document.
    #[error("no extractable content: {0}")]
    NoContent(String),
    /// The extractor itself failed.
    #[error("extractor failed: {0}")]
    Failed(String),
}

impl ExtractError {
    /// Retry class (§10): a failed extractor may succeed on retry; missing content
    /// is a property of the document.
    #[must_use]
    pub fn class(&self) -> Class {
        match self {
            ExtractError::NoContent(_) => Class::Permanent,
            ExtractError::Failed(_) => Class::Retry,
        }
    }
}

/// The extraction port (§9): HTML → (title, byline, markdown). Pure; relative URLs
/// absolutized; scripts stripped before conversion (§12).
pub trait Extractor: Send + Sync {
    /// Extracts the primary content of `html`, resolving relative URLs against
    /// `base_url`.
    ///
    /// # Errors
    /// [`ExtractError`] — never quality outcomes (§10).
    fn extract(&self, html: &str, base_url: &str) -> Result<ExtractedArticle, ExtractError>;
}

/// Runs the stage for one claimed job.
///
/// # Errors
/// [`StageError`] classified per §10.
pub(super) fn run(_config: &Config, _job: &ClaimedJob) -> Result<(), StageError> {
    Err(StageError::Permanent {
        reason: "stage 2 (clean) is not implemented yet (§15 step 3)".to_string(),
    })
}
