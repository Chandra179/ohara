//! Stage 3a — Chunk (§8): header-aware split → recursive fallback with overlap →
//! breadcrumbs, budgeted in the embedder's tokenizer. Lands with the §15 step 4
//! build step.

use crate::config::Config;
use crate::control::ClaimedJob;

use super::StageError;

/// Runs the stage for one claimed job.
///
/// # Errors
/// [`StageError`] classified per §10.
pub(super) fn run(_config: &Config, _job: &ClaimedJob) -> Result<super::StageOutcome, StageError> {
    Err(StageError::Permanent {
        reason: "stage 3 (chunk & vectorize) is not implemented yet (§15 step 4)".to_string(),
    })
}
