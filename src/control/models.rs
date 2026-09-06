//! Row types that cross module boundaries. The facade re-exports the public ones;
//! fields stay `pub(crate)` — construction goes through `control` functions (§6).

/// Pipeline stages — one job row per `(doc_id, stage)` (§6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stage {
    /// Stage 1 — fetch and store raw HTML (§8).
    Scrape,
    /// Stage 2 — clean, dedup, quality + language gate (§8).
    Clean,
    /// Stage 3 — chunk and vectorize (§8).
    Vectorize,
    /// Stage 4 — triplet extraction and entity resolution (§8).
    Extract,
}

impl Stage {
    /// Pipeline order (§6: SCRAPE → CLEAN → VECTORIZE → EXTRACT).
    pub const ALL: [Stage; 4] = [
        Stage::Scrape,
        Stage::Clean,
        Stage::Vectorize,
        Stage::Extract,
    ];

    /// The `jobs.stage` discriminator — matches the §5 CHECK constraint.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Scrape => "SCRAPE",
            Stage::Clean => "CLEAN",
            Stage::Vectorize => "VECTORIZE",
            Stage::Extract => "EXTRACT",
        }
    }
}

/// A job atomically claimed by the worker (the §6 claim SQL's `RETURNING` row).
#[derive(Debug, Clone)]
pub struct ClaimedJob {
    pub(crate) job_id: String,
    pub(crate) doc_id: String,
    pub(crate) attempts: i64,
    pub(crate) max_attempts: i64,
    pub(crate) params: Option<String>,
}

impl ClaimedJob {
    /// Stable job identifier (uuidv7).
    #[must_use]
    pub fn job_id(&self) -> &str {
        &self.job_id
    }

    /// The document this job advances.
    #[must_use]
    pub fn doc_id(&self) -> &str {
        &self.doc_id
    }

    /// Ended executions before this run — attempts count ended runs, not claims (§6).
    #[must_use]
    pub fn attempts(&self) -> i64 {
        self.attempts
    }

    /// Attempts allowed before the job goes `DEAD`.
    #[must_use]
    pub fn max_attempts(&self) -> i64 {
        self.max_attempts
    }

    /// JSON stage params, if any (e.g. `{"embedding_model": "..."}`).
    #[must_use]
    pub fn params(&self) -> Option<&str> {
        self.params.as_deref()
    }
}
