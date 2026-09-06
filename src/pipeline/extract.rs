//! Stage 4 — Extract graph (§8): LLM triplet extraction → entity resolution →
//! `:MENTIONS` cross-linking. Lands with the §15 step 6 build step.

use crate::config::Config;
use crate::control::ClaimedJob;

use super::StageError;

/// Runs the stage for one claimed job.
///
/// # Errors
/// [`StageError`] classified per §10.
pub(super) fn run(_config: &Config, _job: &ClaimedJob) -> Result<(), StageError> {
    Err(StageError::Permanent {
        reason: "stage 4 (extract graph) is not implemented yet (§15 step 6)".to_string(),
    })
}
