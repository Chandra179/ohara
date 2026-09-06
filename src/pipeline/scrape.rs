//! Stage 1 — Scrape (§8): fetch ladder → `data/raw/<doc_id>.html.gz` → `SCRAPED`.
//! The fetcher legs, URL normalization, and politeness machinery land with the
//! §15 step 3 build step; until then the stage rejects jobs honestly.

use crate::config::Config;
use crate::control::ClaimedJob;

use super::StageError;

/// Runs the stage for one claimed job.
///
/// # Errors
/// [`StageError`] classified per §10.
pub(super) fn run(_config: &Config, _job: &ClaimedJob) -> Result<(), StageError> {
    Err(StageError::Permanent {
        reason: "stage 1 (scrape) is not implemented yet (§15 step 3)".to_string(),
    })
}
