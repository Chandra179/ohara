//! Integration (§14): the control-plane flows of §15 step 2 through the public
//! library API only — enqueue → stage chaining to INDEXED, terminal outcomes,
//! requeue recovery, and the §7.6 deletion cascade (FTS included).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // §10: tests unwrap freely

use ohara::control::{self, Completion, DocStatus, EnqueueOutcome, NewDocument, Stage};

const NOW: &str = "2026-09-06 12:00:00";

/// Boots a store in a temp dir; returns (guard, connection).
fn boot() -> (tempfile::TempDir, rusqlite::Connection) {
    let dir = tempfile::tempdir().unwrap();
    let conn = control::connect(&dir.path().join("ohara.db")).unwrap();
    (dir, conn)
}

/// Registers a document via the public API; returns its id.
fn enqueue(conn: &rusqlite::Connection, url: &str) -> String {
    control::insert_new(
        conn,
        std::path::Path::new("data"),
        &NewDocument {
            source_url: url.to_string(),
            source_url_normalized: url.to_string(),
            priority: 5,
            pipeline_version: "0.1.0".to_string(),
        },
        NOW,
    )
    .unwrap()
    .doc_id()
    .to_string()
}

#[test]
fn enqueue_chains_a_document_through_every_milestone() {
    let (_dir, conn) = boot();
    let doc_id = enqueue(&conn, "https://example.com/article");

    let doc = control::get(&conn, &doc_id).unwrap().unwrap();
    assert_eq!(doc.status, DocStatus::New);
    assert_eq!(
        doc.raw_file_path,
        format!("data/raw/{doc_id}.html.gz"),
        "§3: the raw payload path is derived from the document id"
    );
    assert_eq!(
        doc.doc_id.len(),
        36,
        "uuidv7 identity (§3): the claim-ordering tiebreak depends on it"
    );

    // Drive the §6 chain: each stage's DONE carries the milestone and the next
    // job in one transaction — no manual enqueueing between stages.
    for stage in [
        (Stage::Scrape, DocStatus::Scraped),
        (Stage::Clean, DocStatus::Cleaned),
        (Stage::Vectorize, DocStatus::Vectorized),
    ] {
        let job = control::claim_next(&conn, stage.0, "w1", NOW, 60)
            .unwrap()
            .unwrap_or_else(|| panic!("{} job chained by its predecessor", stage.0.as_str()));
        assert_eq!(job.doc_id(), doc_id);
        control::complete(&conn, stage.0, &job, Completion::Chain, NOW).unwrap();
        assert_eq!(
            control::get(&conn, &doc_id).unwrap().unwrap().status,
            stage.1,
            "milestone advanced in the completion transaction"
        );
    }

    // EXTRACT is the last stage: DONE sets INDEXED and chains nothing.
    let job = control::claim_next(&conn, Stage::Extract, "w1", NOW, 60)
        .unwrap()
        .expect("EXTRACT job chained");
    control::complete(&conn, Stage::Extract, &job, Completion::Chain, NOW).unwrap();
    let doc = control::get(&conn, &doc_id).unwrap().unwrap();
    assert_eq!(doc.status, DocStatus::Indexed);

    let (pending, total): (i64, i64) = conn
        .query_row(
            "SELECT
                (SELECT count(*) FROM jobs WHERE status = 'PENDING'),
                (SELECT count(*) FROM jobs)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (pending, total),
        (0, 4),
        "§6: exactly one job per (doc, stage), all DONE, chain ends at EXTRACT"
    );
}

#[test]
fn duplicate_enqueue_is_a_value_naming_the_existing_document() {
    let (_dir, conn) = boot();
    let first = enqueue(&conn, "https://example.com/article");

    let second = control::insert_new(
        &conn,
        std::path::Path::new("data"),
        &NewDocument {
            source_url: "https://example.com/article?utm_source=feed".to_string(),
            source_url_normalized: "https://example.com/article".to_string(),
            priority: 5,
            pipeline_version: "0.1.0".to_string(),
        },
        NOW,
    )
    .unwrap();

    assert_eq!(
        second,
        EnqueueOutcome::Duplicate { doc_id: first },
        "§5 URL-level dedup: same normalized URL, same document"
    );
}

#[test]
fn requeue_grants_a_dead_document_a_fresh_attempt_budget() {
    let (_dir, conn) = boot();
    let doc_id = enqueue(&conn, "https://example.com/article");
    let job = control::claim_next(&conn, Stage::Scrape, "w1", NOW, 60)
        .unwrap()
        .expect("claim");
    control::dead(&conn, job.job_id(), &doc_id, "boom", NOW).unwrap();

    let reset = control::requeue(&conn, &doc_id, NOW).unwrap();

    assert_eq!(reset, 1, "the DEAD SCRAPE job was reset");
    let (status, attempts): (String, i64) = conn
        .query_row(
            "SELECT status, attempts FROM jobs WHERE job_id = ?1",
            [job.job_id()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "PENDING");
    assert_eq!(attempts, 0, "§6: requeue resets attempts");
    assert!(
        control::claim_next(&conn, Stage::Scrape, "w1", NOW, 60)
            .unwrap()
            .is_some(),
        "the requeued job is immediately claimable"
    );
}

#[test]
fn boot_sweep_executes_deletion_intents_without_fts_ghosts() {
    let (_dir, conn) = boot();
    let doc_id = enqueue(&conn, "https://example.com/doomed");
    // A chunk exists so the §7.6 cascade exercises the FTS delete trigger.
    conn.execute(
        "INSERT INTO chunks (id, chunk_id, doc_id, seq, text, embed_text, token_count,
                             embedding_model, content_hash)
         VALUES (1, 'c1', ?1, 0, 'SQLite is an embedded database',
                 'SQLite is an embedded database', 6, 'bge-small-en-v1.5', 'h1')",
        [&doc_id],
    )
    .unwrap();

    control::request_deletion(&conn, &doc_id, Some("user request")).unwrap();

    let report = control::reconcile(&conn, NOW, std::time::Duration::ZERO).unwrap();
    assert_eq!(
        report.deletions_executed, 1,
        "§7.6: the boot sweep finishes interrupted deletions"
    );
    assert!(control::get(&conn, &doc_id).unwrap().is_none());

    let (fts_hits, chunks_left, jobs_left): (i64, i64, i64) = conn
        .query_row(
            "SELECT
                (SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH 'embedded'),
                (SELECT count(*) FROM chunks),
                (SELECT count(*) FROM jobs)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (fts_hits, chunks_left, jobs_left),
        (0, 0, 0),
        "CASCADE removed jobs and chunks; the FTS triggers fired on the cascade"
    );

    // Idempotent: a second sweep finds nothing to do.
    let again = control::reconcile(&conn, NOW, std::time::Duration::ZERO).unwrap();
    assert_eq!(again.deletions_executed, 0);
}
