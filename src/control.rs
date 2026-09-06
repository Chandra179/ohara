//! CONTROL PLANE facade (§1.3) — owns all `SQLite` access: documents, the job queue,
//! the audit trail, and the schema. One directory owns the store; schema changes
//! touch one place. Pipeline code sees only this facade.

mod db;
mod documents;
mod jobs;
mod models;
mod reconcile;
mod sites;

pub use db::{DbError, connect};
pub use documents::{
    ChunkSignature, ChunkText, CleanResult, DeletionIntent, Document, EnqueueOutcome, NewChunkRow,
    NewDocument, chunk_signatures, chunks_by_ids, due_for_recrawl, execute_deletion,
    find_id_by_content_hash, find_id_by_url, get, insert_new, mark_quality_rejected,
    pending_deletions, replace_chunks, request_deletion, update_clean_result, update_fetch_result,
    update_vectorize_result,
};
pub use jobs::{claim_next, complete, dead, enqueue, record_event, requeue, retry};
pub use models::{ClaimedJob, Completion, DocStatus, Stage};
pub use reconcile::{ReconcileReport, reconcile};
pub use sites::{LadderHint, SitePolicy, get as site_policy, set as set_site_policy};

pub(crate) use db::{now, now_plus};

/// Shared in-crate fixtures for the control-plane unit tests (§10: tests unwrap
/// freely).
#[cfg(test)]
pub(crate) mod testing {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::Path;

    use rusqlite::Connection;

    use super::documents::{self, NewDocument};

    /// Boots a fresh in-memory control store.
    pub(crate) fn boot() -> Connection {
        super::connect(Path::new(":memory:")).unwrap()
    }

    /// Registers one document (URL-level dedup satisfied either way: a duplicate
    /// still resolves to the registered id) with its SCRAPE job `PENDING`.
    pub(crate) fn seed_doc(conn: &Connection, slug: &str) -> String {
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
