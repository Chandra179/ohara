//! Row types that cross module boundaries, and the §6 state machine's shape
//! knowledge (stage order, milestones). The facade re-exports the public ones;
//! fields stay `pub(crate)` — construction goes through `control` functions (§6).

/// Priority used when a legacy document has no SCRAPE job row to inherit from.
pub(crate) const DEFAULT_JOB_PRIORITY: i64 = 5;

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

    /// The next stage in the pipeline (§6 chaining); `None` after EXTRACT.
    #[must_use]
    pub fn successor(self) -> Option<Stage> {
        match self {
            Stage::Scrape => Some(Stage::Clean),
            Stage::Clean => Some(Stage::Vectorize),
            Stage::Vectorize => Some(Stage::Extract),
            Stage::Extract => None,
        }
    }

    /// The document milestone this stage's `DONE` sets (§5: the doc stays at its
    /// last *completed* milestone; EXTRACT completes at `INDEXED`).
    #[must_use]
    pub fn milestone(self) -> &'static str {
        match self {
            Stage::Scrape => "SCRAPED",
            Stage::Clean => "CLEANED",
            Stage::Vectorize => "VECTORIZED",
            Stage::Extract => "INDEXED",
        }
    }
}

/// What a `DONE` job means for its document and the chain (§6 stage chaining).
/// Decided by the worker — it owns the configuration (`graph_enabled`) and the
/// stage's reported outcome; applied by [`crate::control::complete`] in one
/// transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    /// Advance the milestone and enqueue the successor stage's `PENDING` job in
    /// the same transaction (§6). With no successor (EXTRACT), only the milestone
    /// applies.
    Chain,
    /// Final milestone, no successor job: the graph is disabled and the chain ends
    /// at VECTORIZE (§6) — documents stay `VECTORIZED` and remain retrievable via
    /// BM25 + vector.
    Milestone,
    /// Job `DONE` only — the stage recorded the document's terminal domain outcome
    /// itself (§8 Stage 2: `FAILED_QUALITY` rejection, duplicate skip); nothing is
    /// chained and the milestone is not advanced.
    Done,
}

/// Document registry states (§5 `documents.status`). Milestones mark what has
/// *completed*; `NEW` is the birth state (enqueued, nothing completed yet);
/// `FAILED*`/`ARCHIVED` are terminal-ish outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DocStatus {
    /// Enqueued, awaiting its SCRAPE job; no milestone completed yet.
    New,
    /// Stage 1 done — raw payload stored (§8 Stage 1).
    Scraped,
    /// Stage 2 done — cleaned Markdown stored (§8 Stage 2).
    Cleaned,
    /// Stage 3 done — chunked and vectorized (§8 Stage 3).
    Vectorized,
    /// Stage 4 done — graph extracted; fully indexed (§8 Stage 4).
    Indexed,
    /// Stage 2 quality gate rejected the document (§8 Stage 2).
    FailedQuality,
    /// A job went `DEAD` (§6 terminal mapping).
    Failed,
    /// User-marked retention state; chunks stay queryable (§6).
    Archived,
}

impl DocStatus {
    /// The `documents.status` discriminator — matches the §5 CHECK constraint.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            DocStatus::New => "NEW",
            DocStatus::Scraped => "SCRAPED",
            DocStatus::Cleaned => "CLEANED",
            DocStatus::Vectorized => "VECTORIZED",
            DocStatus::Indexed => "INDEXED",
            DocStatus::FailedQuality => "FAILED_QUALITY",
            DocStatus::Failed => "FAILED",
            DocStatus::Archived => "ARCHIVED",
        }
    }
}

impl std::str::FromStr for DocStatus {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "NEW" => DocStatus::New,
            "SCRAPED" => DocStatus::Scraped,
            "CLEANED" => DocStatus::Cleaned,
            "VECTORIZED" => DocStatus::Vectorized,
            "INDEXED" => DocStatus::Indexed,
            "FAILED_QUALITY" => DocStatus::FailedQuality,
            "FAILED" => DocStatus::Failed,
            "ARCHIVED" => DocStatus::Archived,
            _ => return Err(()),
        })
    }
}

/// One immutable audit record from `stage_events` (§5, §13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageEvent {
    /// `SQLite` event id, ordered by insertion.
    pub event_id: i64,
    /// The document involved, when the event belongs to a document.
    pub doc_id: Option<String>,
    /// The job involved, when the event belongs to a job.
    pub job_id: Option<String>,
    /// The stage involved, when the event belongs to a stage.
    pub stage: Option<String>,
    /// The recorded outcome, such as `DONE`, `RETRY`, `DEAD`, `FATAL`, `PANIC`,
    /// or `SKIP`.
    pub outcome: String,
    /// Error chain or decision detail, when recorded.
    pub detail: Option<String>,
    /// UTC timestamp in `SQLite`'s native format (§5).
    pub ts: String,
}

/// A job atomically claimed by the worker (the §6 claim SQL's `RETURNING` row).
#[derive(Debug, Clone)]
pub struct ClaimedJob {
    pub(crate) job_id: String,
    pub(crate) doc_id: String,
    pub(crate) attempts: i64,
    pub(crate) max_attempts: i64,
    pub(crate) priority: i64,
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

    /// Enqueue priority (§6) — inherited by the chained successor job.
    #[must_use]
    pub fn priority(&self) -> i64 {
        self.priority
    }

    /// JSON stage params, if any (e.g. `{"embedding_model": "..."}`).
    #[must_use]
    pub fn params(&self) -> Option<&str> {
        self.params.as_deref()
    }
}

/// Mints a uuidv7 document/job id (§3 ID taxonomy: time-ordered; the uuidv7 byte
/// order is the §6 claim-ordering tiebreak). The control plane mints ids itself —
/// stage chaining creates the successor job inside its transaction, where no
/// caller could pre-supply one.
pub(crate) fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}
