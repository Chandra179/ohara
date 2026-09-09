//! Integration (§14): the control-plane flows of §15 step 2 through the public
//! library API only — enqueue → stage chaining to INDEXED, terminal outcomes,
//! requeue recovery, and the §7.6 deletion cascade (FTS included).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // §10: tests unwrap freely

use ohara::control::{
    self, Completion, ControlDb, DocStatus, EnqueueOutcome, NewChunkRow, NewDocument, Stage,
};

const NOW: &str = "2026-09-06 12:00:00";

/// Boots a store in a temp dir; returns (guard, opaque control store).
fn boot() -> (tempfile::TempDir, ControlDb) {
    let dir = tempfile::tempdir().unwrap();
    let conn = control::connect(&dir.path().join("ohara.db")).unwrap();
    (dir, conn)
}

/// Registers a document via the public API; returns its id.
fn enqueue(conn: &ControlDb, url: &str) -> String {
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

    for stage in Stage::ALL {
        assert!(
            control::claim_next(&conn, stage, "w1", NOW, 60)
                .unwrap()
                .is_none(),
            "completed stage queue must not expose another runnable job"
        );
    }
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
    let requeued = control::claim_next(&conn, Stage::Scrape, "w1", NOW, 60)
        .unwrap()
        .expect("the requeued job is immediately claimable");
    assert_eq!(requeued.job_id(), job.job_id());
    assert_eq!(requeued.attempts(), 0, "§6: requeue resets attempts");
}

#[test]
fn boot_sweep_executes_deletion_intents_without_fts_ghosts() {
    let (_dir, conn) = boot();
    let doc_id = enqueue(&conn, "https://example.com/doomed");
    // A chunk exists so the §7.6 cascade exercises the FTS delete trigger.
    control::replace_chunks(
        &conn,
        &doc_id,
        &[NewChunkRow {
            chunk_id: "c1".to_string(),
            seq: 0,
            header_path: "article".to_string(),
            text: "SQLite is an embedded database".to_string(),
            embed_text: "SQLite is an embedded database".to_string(),
            token_count: 6,
            embedding_model: "bge-small-en-v1.5".to_string(),
            content_hash: "h1".to_string(),
        }],
    )
    .unwrap();

    control::request_deletion(&conn, &doc_id, Some("user request")).unwrap();

    assert!(
        control::execute_deletion(&conn, &doc_id).unwrap(),
        "§7.6: the explicit SQLite half finishes after knowledge cleanup"
    );
    assert!(control::get(&conn, &doc_id).unwrap().is_none());

    assert!(
        control::search_bm25(&conn, "embedded", 10)
            .unwrap()
            .is_empty(),
        "CASCADE removed chunks and the FTS index has no ghost"
    );
    assert!(control::pending_deletions(&conn).unwrap().is_empty());

    // Idempotent: a second sweep finds nothing to do.
    assert!(!control::execute_deletion(&conn, &doc_id).unwrap());
}
