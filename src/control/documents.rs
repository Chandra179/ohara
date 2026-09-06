//! Document registry (§5 `documents`): the `ohara enqueue <url>` primitive, dedup
//! lookups, milestone-adjacent outcomes, re-crawl due-dates (§7.5), and the §7.6
//! deletion-intent protocol. Milestone *advances* themselves live in
//! [`super::jobs::complete`] — they must share the chaining transaction.

use std::path::Path;
use std::str::FromStr;

use rusqlite::Connection;

use super::db::DbError;
use super::jobs;
use super::models::{DocStatus, Stage, new_id};

/// A document registration request (`ohara enqueue <url>`, §6): the raw and
/// normalized URL, the operator priority, and the pipeline version stamping the
/// row for reprocessing decisions (§1.2.7).
#[derive(Debug, Clone)]
pub struct NewDocument {
    /// The URL as given.
    pub source_url: String,
    /// The normalized form — the §5 URL-level dedup key (normalization spec in
    /// §8 Stage 1).
    pub source_url_normalized: String,
    /// Priority for the document's jobs; lower = sooner (§6).
    pub priority: i64,
    /// Pipeline version recorded on the row (§5 `pipeline_version`).
    pub pipeline_version: String,
}

/// The outcome of [`insert_new`] — URL-level dedup is a domain *value*, not an
/// error (§10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnqueueOutcome {
    /// Document created and its SCRAPE job enqueued.
    Enqueued {
        /// The minted document id (uuidv7).
        doc_id: String,
        /// The minted SCRAPE job id (uuidv7).
        job_id: String,
    },
    /// `source_url_normalized` is already registered (§5 `UNIQUE`); the existing
    /// document's id is returned so callers can report it.
    Duplicate {
        /// The already-registered document.
        doc_id: String,
    },
}

impl EnqueueOutcome {
    /// The document this outcome refers to — minted on [`EnqueueOutcome::Enqueued`],
    /// already-registered on [`EnqueueOutcome::Duplicate`].
    #[must_use]
    pub fn doc_id(&self) -> &str {
        match self {
            EnqueueOutcome::Enqueued { doc_id, .. } | EnqueueOutcome::Duplicate { doc_id } => {
                doc_id
            }
        }
    }
}

/// A `documents` row (§5). Plain data record — no invariant beyond construction
/// (`CODE_GUIDE` §6 DTO exception); the store's CHECK constraints guarantee validity.
#[derive(Debug, Clone)]
pub struct Document {
    /// uuidv7 (time-ordered).
    pub doc_id: String,
    /// The URL as given.
    pub source_url: String,
    /// The dedup key (§5).
    pub source_url_normalized: String,
    /// Where the raw payload will live — `data/raw/<doc_id>.html.gz` (§3).
    pub raw_file_path: String,
    /// Where the cleaned Markdown lives (§3), once Stage 2 completes.
    pub clean_file_path: Option<String>,
    /// Content-level dedup key (§8 Stage 2).
    pub clean_content_hash: Option<String>,
    /// Last completed milestone (§5 note; v1's execution states live in `jobs`).
    pub status: DocStatus,
    /// Page title, from Stage 2 extraction.
    pub title: Option<String>,
    /// Byline, from Stage 2 extraction.
    pub author: Option<String>,
    /// whatlang result; gated at Stage 2 (§8).
    pub language: Option<String>,
    /// Clean-text word count.
    pub word_count: Option<i64>,
    /// Clean-text token count (embedder tokenizer, §4).
    pub token_count: Option<i64>,
    /// Chunks written by Stage 3.
    pub chunk_count: i64,
    /// Last fetch's HTTP status.
    pub http_status: Option<i64>,
    /// Conditional re-crawl validators (§7.5).
    pub etag: Option<String>,
    /// Conditional re-crawl validators (§7.5).
    pub last_modified: Option<String>,
    /// Last fetch time (§5 timestamp rule).
    pub fetched_at: Option<String>,
    /// Next re-crawl due date; `None` = never (§7.5).
    pub next_crawl_at: Option<String>,
    /// Row creation time.
    pub created_at: String,
    /// Set on each transition (§5).
    pub last_processed_at: Option<String>,
    /// Last failure detail.
    pub error: Option<String>,
    /// Reprocess when logic changes (§1.2.7).
    pub pipeline_version: String,
}

/// Registers a document and enqueues its SCRAPE job as one transaction (§6:
/// `SCRAPE` jobs are created by `ohara enqueue <url>`). The row is born `NEW` —
/// no milestone completed yet — with `raw_file_path` precomputed (the payload
/// path is derived from the id, §3); Stage 1 fills it. The SCRAPE job inherits
/// `priority`.
///
/// # Errors
/// [`DbError::Sqlite`] on statement failure (the transaction rolls back).
pub fn insert_new(
    conn: &Connection,
    data_dir: &Path,
    new: &NewDocument,
    now_stamp: &str,
) -> Result<EnqueueOutcome, DbError> {
    let tx = conn.unchecked_transaction()?;
    let doc_id = new_id();
    let raw_file_path = data_dir.join("raw").join(format!("{doc_id}.html.gz"));
    let inserted = tx.execute(
        "INSERT INTO documents (doc_id, source_url, source_url_normalized, raw_file_path,
                                status, pipeline_version)
         VALUES (?1, ?2, ?3, ?4, 'NEW', ?5)",
        rusqlite::params![
            doc_id,
            new.source_url,
            new.source_url_normalized,
            raw_file_path.to_string_lossy(),
            new.pipeline_version,
        ],
    );
    if let Err(rusqlite::Error::SqliteFailure(ffi, Some(msg))) = &inserted {
        // URL-level dedup is a value outcome (§10): the §5 UNIQUE key collided.
        // The violating row must exist; if the lookup somehow missed, the
        // original constraint error propagates below.
        if ffi.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
            && msg.contains("source_url_normalized")
            && let Some(existing) = find_id_by_url(&tx, &new.source_url_normalized)?
        {
            return Ok(EnqueueOutcome::Duplicate { doc_id: existing });
        }
    }
    inserted?;
    let job_id = new_id();
    jobs::enqueue(
        &tx,
        &job_id,
        &doc_id,
        Stage::Scrape,
        new.priority,
        None,
        now_stamp,
    )?;
    tx.commit()?;
    Ok(EnqueueOutcome::Enqueued { doc_id, job_id })
}

/// Looks up a document id by its normalized URL (the §5 URL-level dedup key).
///
/// # Errors
/// [`DbError::Sqlite`] on statement failure.
pub fn find_id_by_url(
    conn: &Connection,
    source_url_normalized: &str,
) -> Result<Option<String>, DbError> {
    let mut stmt = conn.prepare("SELECT doc_id FROM documents WHERE source_url_normalized = ?1")?;
    let mut rows = stmt.query([source_url_normalized])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

/// Looks up a document id by clean-content hash (§8 Stage 2 content-level dedup:
/// `SkippedDuplicate` when found).
///
/// # Errors
/// [`DbError::Sqlite`] on statement failure.
pub fn find_id_by_content_hash(
    conn: &Connection,
    clean_content_hash: &str,
) -> Result<Option<String>, DbError> {
    let mut stmt = conn.prepare("SELECT doc_id FROM documents WHERE clean_content_hash = ?1")?;
    let mut rows = stmt.query([clean_content_hash])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

/// Loads one document row.
///
/// # Errors
/// [`DbError::Sqlite`] on statement failure.
///
/// # Panics
/// Never in practice: a status violating the §5 CHECK cannot be stored; reading
/// one would be a post-migration schema mismatch — a broken internal invariant (§10).
pub fn get(conn: &Connection, doc_id: &str) -> Result<Option<Document>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT doc_id, source_url, source_url_normalized, raw_file_path, clean_file_path,
                clean_content_hash, status, title, author, language, word_count, token_count,
                chunk_count, http_status, etag, last_modified, fetched_at, next_crawl_at,
                created_at, last_processed_at, error, pipeline_version
           FROM documents WHERE doc_id = ?1",
    )?;
    let mut rows = stmt.query([doc_id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let status: String = row.get(6)?;
    // §10: a status outside the §5 CHECK cannot be stored — reading one is a
    // post-migration schema mismatch, a broken internal invariant. The failure is
    // surfaced loudly here rather than laundered into a fake value.
    #[allow(clippy::expect_used)]
    let status = DocStatus::from_str(&status)
        .expect("invariant: documents.status satisfies the §5 CHECK constraint");
    Ok(Some(Document {
        doc_id: row.get(0)?,
        source_url: row.get(1)?,
        source_url_normalized: row.get(2)?,
        raw_file_path: row.get(3)?,
        clean_file_path: row.get(4)?,
        clean_content_hash: row.get(5)?,
        status,
        title: row.get(7)?,
        author: row.get(8)?,
        language: row.get(9)?,
        word_count: row.get(10)?,
        token_count: row.get(11)?,
        chunk_count: row.get(12)?,
        http_status: row.get(13)?,
        etag: row.get(14)?,
        last_modified: row.get(15)?,
        fetched_at: row.get(16)?,
        next_crawl_at: row.get(17)?,
        created_at: row.get(18)?,
        last_processed_at: row.get(19)?,
        error: row.get(20)?,
        pipeline_version: row.get(21)?,
    }))
}

/// Records a Stage 2 quality-gate rejection (§8): the document goes
/// `FAILED_QUALITY` with the reason stored as its `error`. A domain outcome the
/// stage body applies itself — the job still completes (`DONE`), nothing chains.
///
/// # Errors
/// [`DbError::Sqlite`] on statement failure.
pub fn mark_quality_rejected(
    conn: &Connection,
    doc_id: &str,
    reason: &str,
    now_stamp: &str,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE documents
            SET status = 'FAILED_QUALITY', error = ?2, last_processed_at = ?3
          WHERE doc_id = ?1",
        rusqlite::params![doc_id, reason, now_stamp],
    )?;
    Ok(())
}

/// Documents due for re-crawl (§7.5): `next_crawl_at <= now`, excluding `FAILED*`
/// and `ARCHIVED` (never re-crawled). `NULL` `next_crawl_at` means never.
///
/// # Errors
/// [`DbError::Sqlite`] on statement failure.
pub fn due_for_recrawl(conn: &Connection, now_stamp: &str) -> Result<Vec<String>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT doc_id FROM documents
          WHERE next_crawl_at IS NOT NULL AND next_crawl_at <= ?1
            AND status NOT IN ('FAILED_QUALITY', 'FAILED', 'ARCHIVED')
          ORDER BY next_crawl_at",
    )?;
    let mut rows = stmt.query([now_stamp])?;
    let mut due = Vec::new();
    while let Some(row) = rows.next()? {
        due.push(row.get(0)?);
    }
    Ok(due)
}

/// A pending §7.6 deletion intent: the document row must go, and the knowledge
/// plane must forget it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletionIntent {
    /// The document marked for deletion.
    pub doc_id: String,
    /// Why, if the requester said.
    pub reason: Option<String>,
}

/// Records deletion *intent* first (§7.6): the `deletions` row exists exactly
/// while the document does, so an interrupted deletion re-executes idempotently
/// at boot ([`super::reconcile`]).
///
/// # Errors
/// [`DbError::Sqlite`] on statement failure (an unknown `doc_id` violates the FK).
pub fn request_deletion(
    conn: &Connection,
    doc_id: &str,
    reason: Option<&str>,
) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO deletions (doc_id, reason) VALUES (?1, ?2)",
        rusqlite::params![doc_id, reason],
    )?;
    Ok(())
}

/// Reads all pending deletion intents (§7.6), oldest first.
///
/// # Errors
/// [`DbError::Sqlite`] on statement failure.
pub fn pending_deletions(conn: &Connection) -> Result<Vec<DeletionIntent>, DbError> {
    let mut stmt = conn.prepare("SELECT doc_id, reason FROM deletions ORDER BY requested_at")?;
    let mut rows = stmt.query([])?;
    let mut pending = Vec::new();
    while let Some(row) = rows.next()? {
        pending.push(DeletionIntent {
            doc_id: row.get(0)?,
            reason: row.get(1)?,
        });
    }
    Ok(pending)
}

/// Executes a deletion's `SQLite` half (§7.6): one statement deletes the `documents`
/// row — the CASCADEs remove its jobs, chunks (their FTS entries go with them),
/// triplets, and the intent row itself. The knowledge-plane `delete_doc` runs
/// *before* this (§7.6 intent-before-write); callers performing both halves must
/// order them that way. Returns whether a row was deleted.
///
/// # Errors
/// [`DbError::Sqlite`] on statement failure.
pub fn execute_deletion(conn: &Connection, doc_id: &str) -> Result<bool, DbError> {
    let deleted = conn.execute("DELETE FROM documents WHERE doc_id = ?1", [doc_id])?;
    Ok(deleted == 1)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // §10: tests unwrap freely

    use std::path::Path;

    use super::super::jobs;
    use super::super::models::Completion;
    use super::super::models::Stage;
    use super::super::testing::boot;
    use super::*;

    const NOW: &str = "2026-09-06 12:00:00";

    fn new_doc(slug: &str) -> NewDocument {
        NewDocument {
            source_url: format!("https://example.com/{slug}?utm_source=x"),
            source_url_normalized: format!("https://example.com/{slug}"),
            priority: 5,
            pipeline_version: "0.1.0".to_string(),
        }
    }

    #[test]
    fn insert_new_creates_a_new_document_with_its_scrape_job() {
        let conn = boot();

        let outcome = insert_new(&conn, Path::new("data"), &new_doc("a"), NOW).unwrap();

        let EnqueueOutcome::Enqueued { doc_id, job_id } = outcome else {
            panic!("fresh store must enqueue");
        };
        let doc = get(&conn, &doc_id).unwrap().expect("row exists");
        assert_eq!(doc.status, DocStatus::New, "no milestone completed yet");
        assert_eq!(
            doc.raw_file_path,
            format!("data/raw/{doc_id}.html.gz"),
            "§3: the payload path is derived from the id"
        );
        assert_eq!(doc.chunk_count, 0);
        let (stage, status, priority): (String, String, i64) = conn
            .query_row(
                "SELECT stage, status, priority FROM jobs WHERE job_id = ?1",
                [&job_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (stage.as_str(), status.as_str(), priority),
            ("SCRAPE", "PENDING", 5)
        );
    }

    #[test]
    fn insert_new_reports_a_duplicate_url_as_a_value() {
        let conn = boot();
        let first = match insert_new(&conn, Path::new("data"), &new_doc("a"), NOW).unwrap() {
            EnqueueOutcome::Enqueued { doc_id, .. } => doc_id,
            EnqueueOutcome::Duplicate { .. } => panic!("first insert enqueues"),
        };

        let second = insert_new(&conn, Path::new("data"), &new_doc("a"), NOW).unwrap();

        assert_eq!(
            second,
            EnqueueOutcome::Duplicate {
                doc_id: first.clone()
            },
            "§10: dedup is a domain outcome, not an error"
        );
        let (docs, jobs): (i64, i64) = conn
            .query_row(
                "SELECT (SELECT count(*) FROM documents), (SELECT count(*) FROM jobs)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((docs, jobs), (1, 1), "the duplicate registered nothing");
    }

    #[test]
    fn lookups_resolve_by_url_and_content_hash() {
        let conn = boot();
        let doc_id = match insert_new(&conn, Path::new("data"), &new_doc("a"), NOW).unwrap() {
            EnqueueOutcome::Enqueued { doc_id, .. } => doc_id,
            EnqueueOutcome::Duplicate { .. } => panic!("fresh store"),
        };

        assert_eq!(
            find_id_by_url(&conn, "https://example.com/a")
                .unwrap()
                .as_deref(),
            Some(doc_id.as_str())
        );
        assert_eq!(
            find_id_by_url(&conn, "https://example.com/z").unwrap(),
            None
        );
        assert_eq!(find_id_by_content_hash(&conn, "nope").unwrap(), None);
        conn.execute(
            "UPDATE documents SET clean_content_hash = 'h1' WHERE doc_id = ?1",
            [&doc_id],
        )
        .unwrap();
        assert_eq!(
            find_id_by_content_hash(&conn, "h1").unwrap().as_deref(),
            Some(doc_id.as_str())
        );
    }

    #[test]
    fn quality_rejection_records_failed_quality_with_the_reason() {
        let conn = boot();
        let doc_id = super::super::testing::seed_doc(&conn, "a");

        mark_quality_rejected(&conn, &doc_id, "paywall markers", NOW).unwrap();

        let doc = get(&conn, &doc_id).unwrap().unwrap();
        assert_eq!(doc.status, DocStatus::FailedQuality);
        assert_eq!(doc.error.as_deref(), Some("paywall markers"));
        assert_eq!(doc.last_processed_at.as_deref(), Some(NOW));
    }

    #[test]
    fn due_for_recrawl_excludes_terminal_and_undated_documents() {
        let conn = boot();
        let due_id = super::super::testing::seed_doc(&conn, "due");
        let future_id = super::super::testing::seed_doc(&conn, "future");
        let never_id = super::super::testing::seed_doc(&conn, "never");
        let failed_id = super::super::testing::seed_doc(&conn, "failed");
        conn.execute(
            "UPDATE documents SET next_crawl_at = '2026-09-06 11:00:00' WHERE doc_id = ?1",
            [&due_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE documents SET next_crawl_at = '2026-09-06 23:00:00' WHERE doc_id = ?1",
            [&future_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE documents SET status = 'FAILED', next_crawl_at = '2026-09-06 11:00:00'
              WHERE doc_id = ?1",
            [&failed_id],
        )
        .unwrap();

        let due = due_for_recrawl(&conn, NOW).unwrap();

        assert_eq!(
            due,
            vec![due_id],
            "§7.5: past-due only; FAILED and NULL never"
        );
        let _ = (future_id, never_id);
    }

    #[test]
    fn deletion_intent_executes_with_cascades_and_disappears() {
        let conn = boot();
        let doc_id = super::super::testing::seed_doc(&conn, "a");
        // A chunk exists so the §7.6 cascade (and its FTS trigger) is exercised.
        conn.execute(
            "INSERT INTO chunks (id, chunk_id, doc_id, seq, text, embed_text, token_count,
                                 embedding_model, content_hash)
             VALUES (1, 'c1', ?1, 0, 'SQLite is embedded', 'SQLite is embedded', 3,
                     'bge-small-en-v1.5', 'h1')",
            [&doc_id],
        )
        .unwrap();

        request_deletion(&conn, &doc_id, Some("user request")).unwrap();
        assert_eq!(pending_deletions(&conn).unwrap().len(), 1);

        assert!(execute_deletion(&conn, &doc_id).unwrap());

        assert!(get(&conn, &doc_id).unwrap().is_none());
        assert_eq!(
            pending_deletions(&conn).unwrap(),
            Vec::<DeletionIntent>::new(),
            "the intent row dies with the document (§7.6)"
        );
        let (jobs_left, chunks_left): (i64, i64) = conn
            .query_row(
                "SELECT (SELECT count(*) FROM jobs), (SELECT count(*) FROM chunks)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (jobs_left, chunks_left),
            (0, 0),
            "CASCADE removed dependents"
        );
        let fts_hits: i64 = conn
            .query_row(
                "SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH 'sqlite'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fts_hits, 0, "no FTS ghost rows after a cascade delete");
    }

    #[test]
    fn deletion_of_an_unknown_document_is_a_foreign_key_error() {
        let conn = boot();
        let err = request_deletion(&conn, "missing", None).unwrap_err();
        assert!(matches!(err, DbError::Sqlite(_)));
    }

    #[test]
    fn milestone_advance_is_jobs_completes_concern_not_documents() {
        // Guardrail: documents.rs exposes no milestone setter — chaining must stay
        // transactional (§6). The round-trip goes through jobs::complete.
        let conn = boot();
        let doc_id = super::super::testing::seed_doc(&conn, "a");
        let job = jobs::claim_next(&conn, Stage::Scrape, "w1", NOW, 60)
            .unwrap()
            .expect("claim");
        jobs::complete(&conn, Stage::Scrape, &job, Completion::Chain, NOW).unwrap();
        assert_eq!(
            get(&conn, &doc_id).unwrap().unwrap().status,
            DocStatus::Scraped
        );
    }
}
