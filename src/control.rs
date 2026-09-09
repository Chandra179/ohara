//! CONTROL PLANE facade (§1.3) — owns all `SQLite` access: documents, the job queue,
//! the audit trail, and the schema. One directory owns the store; schema changes
//! touch one place. Pipeline code sees only this facade.

mod db;
mod documents;
mod entities;
mod jobs;
mod models;
mod reconcile;
mod sites;

pub use db::{ControlDb, DbError, backup_to, connect};
pub use documents::{
    ChunkSignature, ChunkText, CleanResult, DeletionIntent, Document, EnqueueOutcome, NewChunkRow,
    NewDocument,
};
pub use entities::{NameCandidate, NewTriplet, TripletRow};
pub use models::{ClaimedJob, Completion, DocStatus, Stage, StageEvent};
pub use reconcile::ReconcileReport;
pub use sites::{LadderHint, SitePolicy};

/// Registers a document and its initial SCRAPE job.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane write fails.
pub fn insert_new(
    db: &ControlDb,
    data_dir: &std::path::Path,
    new: &NewDocument,
    now_stamp: &str,
) -> Result<EnqueueOutcome, DbError> {
    documents::insert_new(db.raw(), data_dir, new, now_stamp)
}

/// Looks up a document by normalized URL.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane query fails.
pub fn find_id_by_url(
    db: &ControlDb,
    source_url_normalized: &str,
) -> Result<Option<String>, DbError> {
    documents::find_id_by_url(db.raw(), source_url_normalized)
}

/// Looks up a document by clean-content hash.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane query fails.
pub fn find_id_by_content_hash(
    db: &ControlDb,
    clean_content_hash: &str,
) -> Result<Option<String>, DbError> {
    documents::find_id_by_content_hash(db.raw(), clean_content_hash)
}

/// Loads one document row.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane query fails.
pub fn get(db: &ControlDb, doc_id: &str) -> Result<Option<Document>, DbError> {
    documents::get(db.raw(), doc_id)
}

/// Records a Stage 2 quality rejection.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane write fails.
pub fn mark_quality_rejected(
    db: &ControlDb,
    doc_id: &str,
    reason: &str,
    now_stamp: &str,
) -> Result<(), DbError> {
    documents::mark_quality_rejected(db.raw(), doc_id, reason, now_stamp)
}

/// Returns documents due for re-crawl.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane query fails.
pub fn due_for_recrawl(db: &ControlDb, now_stamp: &str) -> Result<Vec<String>, DbError> {
    documents::due_for_recrawl(db.raw(), now_stamp)
}

/// Records deletion intent.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane write fails.
pub fn request_deletion(db: &ControlDb, doc_id: &str, reason: Option<&str>) -> Result<(), DbError> {
    documents::request_deletion(db.raw(), doc_id, reason)
}

/// Reads pending deletion intents.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane query fails.
pub fn pending_deletions(db: &ControlDb) -> Result<Vec<DeletionIntent>, DbError> {
    documents::pending_deletions(db.raw())
}

/// Deletes the SQLite half of a deletion intent.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane write fails.
pub fn execute_deletion(db: &ControlDb, doc_id: &str) -> Result<bool, DbError> {
    documents::execute_deletion(db.raw(), doc_id)
}

/// Records a Stage 1 fetch result.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane write fails.
pub fn update_fetch_result(
    db: &ControlDb,
    doc_id: &str,
    http_status: i64,
    etag: Option<&str>,
    last_modified: Option<&str>,
    now_stamp: &str,
) -> Result<(), DbError> {
    documents::update_fetch_result(
        db.raw(),
        doc_id,
        http_status,
        etag,
        last_modified,
        now_stamp,
    )
}

/// Records a Stage 2 clean result.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane write fails.
pub fn update_clean_result(
    db: &ControlDb,
    doc_id: &str,
    result: &CleanResult,
    now_stamp: &str,
) -> Result<(), DbError> {
    documents::update_clean_result(db.raw(), doc_id, result, now_stamp)
}

/// Reads chunk replay signatures.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane query fails.
pub fn chunk_signatures(db: &ControlDb, doc_id: &str) -> Result<Vec<ChunkSignature>, DbError> {
    documents::chunk_signatures(db.raw(), doc_id)
}

/// Replaces all chunks for a document.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane write fails.
pub fn replace_chunks(db: &ControlDb, doc_id: &str, chunks: &[NewChunkRow]) -> Result<(), DbError> {
    documents::replace_chunks(db.raw(), doc_id, chunks)
}

/// Hydrates chunk display text by cross-store ids.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane query fails.
pub fn chunks_by_ids(db: &ControlDb, ids: &[&str]) -> Result<Vec<ChunkText>, DbError> {
    documents::chunks_by_ids(db.raw(), ids)
}

/// Searches the trigger-synced FTS5 index and returns hydrated chunks.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane query fails.
pub fn search_bm25(
    db: &ControlDb,
    expression: &str,
    limit: usize,
) -> Result<Vec<ChunkText>, DbError> {
    documents::search_bm25(db.raw(), expression, limit)
}

/// Records Stage 3 aggregate counts.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane write fails.
pub fn update_vectorize_result(
    db: &ControlDb,
    doc_id: &str,
    chunk_count: i64,
    token_count: i64,
    now_stamp: &str,
) -> Result<(), DbError> {
    documents::update_vectorize_result(db.raw(), doc_id, chunk_count, token_count, now_stamp)
}

/// Stages extraction triplets and returns only newly inserted rows.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane write fails.
pub fn stage_triplets(
    db: &ControlDb,
    triplets: Vec<NewTriplet>,
) -> Result<Vec<NewTriplet>, DbError> {
    entities::stage_triplets(db.raw(), triplets)
}

/// Returns chunks without staged triplets.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane query fails.
pub fn chunks_without_triplets(db: &ControlDb, doc_id: &str) -> Result<Vec<ChunkText>, DbError> {
    entities::chunks_without_triplets(db.raw(), doc_id)
}

/// Looks up an alias for one entity type.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane query fails.
pub fn lookup_alias(
    db: &ControlDb,
    alias: &str,
    entity_type: &str,
) -> Result<Option<String>, DbError> {
    entities::lookup_alias(db.raw(), alias, entity_type)
}

/// Looks up all entity ids for an alias.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane query fails.
pub fn lookup_alias_all_types(db: &ControlDb, alias: &str) -> Result<Vec<String>, DbError> {
    entities::lookup_alias_all_types(db.raw(), alias)
}

/// Registers an entity alias.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane write fails.
pub fn upsert_alias(
    db: &ControlDb,
    alias: &str,
    entity_type: &str,
    entity_id: &str,
) -> Result<Option<String>, DbError> {
    entities::upsert_alias(db.raw(), alias, entity_type, entity_id)
}

/// Ensures an entity exists and returns its stable id.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane write fails.
pub fn ensure_entity(
    db: &ControlDb,
    entity_id: &str,
    canonical_name: &str,
    entity_type: &str,
    subtype: Option<&str>,
) -> Result<String, DbError> {
    entities::ensure_entity(db.raw(), entity_id, canonical_name, entity_type, subtype)
}

/// Reads an entity's supertype.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane query fails.
pub fn entity_type_of(db: &ControlDb, entity_id: &str) -> Result<Option<String>, DbError> {
    entities::entity_type_of(db.raw(), entity_id)
}

/// Reads an entity's canonical name.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane query fails.
pub fn canonical_name(db: &ControlDb, entity_id: &str) -> Result<Option<String>, DbError> {
    entities::canonical_name(db.raw(), entity_id)
}

/// Reads canonical names for one entity supertype.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane query fails.
pub fn canonical_names(db: &ControlDb, entity_type: &str) -> Result<Vec<NameCandidate>, DbError> {
    entities::canonical_names(db.raw(), entity_type)
}

/// Files a cross-document entity-resolution review candidate.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane write fails.
pub fn er_review_candidate(
    db: &ControlDb,
    entity_a: &str,
    entity_b: &str,
    score: f64,
) -> Result<bool, DbError> {
    entities::er_review_candidate(db.raw(), entity_a, entity_b, score)
}

/// Reads staged triplets for a document.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane query fails.
pub fn triplets_of_doc(db: &ControlDb, doc_id: &str) -> Result<Vec<TripletRow>, DbError> {
    entities::triplets_of_doc(db.raw(), doc_id)
}

/// Claims the next runnable job.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane transaction fails.
pub fn claim_next(
    db: &ControlDb,
    stage: Stage,
    worker: &str,
    now_stamp: &str,
    lease_secs: u64,
) -> Result<Option<ClaimedJob>, DbError> {
    jobs::claim_next(db.raw(), stage, worker, now_stamp, lease_secs)
}

/// Completes a claimed job and applies its milestone/chaining decision.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane transaction fails.
pub fn complete(
    db: &ControlDb,
    stage: Stage,
    job: &ClaimedJob,
    completion: Completion,
    now_stamp: &str,
) -> Result<(), DbError> {
    jobs::complete(db.raw(), stage, job, completion, now_stamp)
}

/// Retries a job with a backoff deadline.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane write fails.
pub fn retry(
    db: &ControlDb,
    job_id: &str,
    due: &str,
    error: &str,
    now_stamp: &str,
) -> Result<(), DbError> {
    jobs::retry(db.raw(), job_id, due, error, now_stamp)
}

/// Marks a job dead and its document failed.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane transaction fails.
pub fn dead(
    db: &ControlDb,
    job_id: &str,
    doc_id: &str,
    error: &str,
    now_stamp: &str,
) -> Result<(), DbError> {
    jobs::dead(db.raw(), job_id, doc_id, error, now_stamp)
}

/// Requeues a document's non-DONE jobs.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane write fails.
pub fn requeue(db: &ControlDb, doc_id: &str, now_stamp: &str) -> Result<usize, DbError> {
    jobs::requeue(db.raw(), doc_id, now_stamp)
}

/// Appends an audit event.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane write fails.
pub fn record_event(
    db: &ControlDb,
    doc_id: Option<&str>,
    job_id: Option<&str>,
    stage: Option<&str>,
    outcome: &str,
    detail: Option<&str>,
) -> Result<(), DbError> {
    jobs::record_event(db.raw(), doc_id, job_id, stage, outcome, detail)
}

/// Reads retained audit events for one document in insertion order.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane query fails.
pub fn events_for_doc(db: &ControlDb, doc_id: &str) -> Result<Vec<StageEvent>, DbError> {
    jobs::events_for_doc(db.raw(), doc_id)
}

/// Enqueues a job.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane write fails.
pub fn enqueue(
    db: &ControlDb,
    job_id: &str,
    doc_id: &str,
    stage: Stage,
    priority: i64,
    params: Option<&str>,
    now_stamp: &str,
) -> Result<(), DbError> {
    jobs::enqueue(db.raw(), job_id, doc_id, stage, priority, params, now_stamp)
}

/// Runs the boot reconciliation sweep.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane sweep fails.
pub fn reconcile(
    db: &ControlDb,
    now_stamp: &str,
    retention: std::time::Duration,
) -> Result<ReconcileReport, DbError> {
    reconcile::reconcile(db.raw(), now_stamp, retention)
}

/// Prunes retained audit events without executing deletion intents.
///
/// This is crate-visible because the worker must first delete a document from
/// the knowledge plane before it removes the SQLite row (§7.6).
pub(crate) fn reconcile_retention(
    db: &ControlDb,
    now_stamp: &str,
    retention: std::time::Duration,
) -> Result<ReconcileReport, DbError> {
    reconcile::reconcile_retention(db.raw(), now_stamp, retention)
}

/// Reads one host policy.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane query fails.
pub fn site_policy(db: &ControlDb, host: &str) -> Result<Option<SitePolicy>, DbError> {
    sites::get(db.raw(), host)
}

/// Upserts one host policy.
///
/// # Errors
///
/// Returns [`DbError`] if the control-plane write fails.
pub fn set_site_policy(
    db: &ControlDb,
    host: &str,
    rate_limit_ms: Option<i64>,
    recrawl_seconds: Option<i64>,
    fetch_hint: Option<LadderHint>,
) -> Result<(), DbError> {
    sites::set(db.raw(), host, rate_limit_ms, recrawl_seconds, fetch_hint)
}

pub(crate) use db::{now, now_plus};
pub(crate) use entities::new_entity_id;

/// Shared in-crate fixtures for the control-plane unit tests (§10: tests unwrap
/// freely).
#[cfg(test)]
pub(crate) mod testing {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::Path;

    use rusqlite::Connection;

    use super::documents::{self, NewDocument};

    /// Boots a fresh in-memory opaque control store for pipeline tests.
    pub(crate) fn boot() -> super::ControlDb {
        super::connect(Path::new(":memory:")).unwrap()
    }

    /// Boots a raw in-memory connection for control-plane implementation tests.
    pub(crate) fn boot_raw() -> Connection {
        super::connect(Path::new(":memory:")).unwrap().into_raw()
    }

    /// Registers one document (URL-level dedup satisfied either way: a duplicate
    /// still resolves to the registered id) with its SCRAPE job `PENDING`.
    pub(crate) fn seed_doc(db: &super::ControlDb, slug: &str) -> String {
        seed_doc_raw(db.raw(), slug)
    }

    /// Registers a document through a raw connection for control-plane unit tests.
    pub(crate) fn seed_doc_raw(conn: &Connection, slug: &str) -> String {
        // Dedup satisfied either way: both arms resolve to the registered id.
        documents::insert_new(
            conn,
            Path::new("data"),
            &NewDocument {
                source_url: format!("https://example.com/{slug}"),
                source_url_normalized: format!("https://example.com/{slug}"),
                priority: 5,
                pipeline_version: "0.1.0".to_string(),
            },
            "2026-09-06 12:00:00",
        )
        .unwrap()
        .doc_id()
        .to_string()
    }
}
