//! Integration (§14): boot the control store via the public library API — pragmas,
//! migrations, schema presence, FTS5 trigger sync, and the §6 claim semantics.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // §10: tests unwrap freely

use ohara::control::{self, Stage};

/// Boots a store in a temp dir; returns (guard, connection).
fn boot() -> (tempfile::TempDir, rusqlite::Connection) {
    let dir = tempfile::tempdir().unwrap();
    let conn = control::connect(&dir.path().join("ohara.db")).unwrap();
    (dir, conn)
}

/// Inserts one document row (jobs reference documents; `foreign_keys` = `ON`).
fn insert_doc(conn: &rusqlite::Connection, doc_id: &str) {
    conn.execute(
        "INSERT INTO documents (doc_id, source_url, source_url_normalized, raw_file_path,
                                status, pipeline_version)
         VALUES (?1, ?2, ?2, ?3, 'SCRAPED', '0.1.0')",
        rusqlite::params![
            doc_id,
            format!("https://example.com/{doc_id}"),
            format!("{doc_id}.html.gz")
        ],
    )
    .unwrap();
}

#[test]
fn boot_applies_migrations_pragmas_and_full_schema() {
    let (_dir, conn) = boot();

    // §5 pragmas on the open connection.
    let foreign_keys: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        foreign_keys, 1,
        "foreign_keys must be ON — the CASCADEs are inert otherwise"
    );
    let journal: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(journal, "wal");
    let synchronous: i64 = conn
        .query_row("PRAGMA synchronous", [], |r| r.get(0))
        .unwrap();
    assert_eq!(synchronous, 1, "NORMAL = 1 (§5)");
    let busy: i64 = conn
        .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
        .unwrap();
    assert_eq!(busy, 5_000);

    // Every §5 table exists (virtual tables included).
    for table in [
        "schema_migrations",
        "sites",
        "documents",
        "jobs",
        "chunks",
        "chunks_fts",
        "entities",
        "entity_aliases",
        "entity_merges",
        "er_review",
        "triplets",
        "deletions",
        "stage_events",
    ] {
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master
                  WHERE name = ?1 AND type IN ('table', 'virtual')",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "table {table} missing");
    }

    // The three FTS trigger sync points exist (§5).
    for trigger in ["chunks_fts_ai", "chunks_fts_ad", "chunks_fts_au"] {
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'trigger' AND name = ?1",
                [trigger],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "trigger {trigger} missing");
    }

    // Migration recorded exactly once.
    let versions: i64 = conn
        .query_row("SELECT count(*) FROM schema_migrations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(versions, 1);
}

#[test]
fn fts5_external_content_index_is_trigger_synced() {
    let (_dir, conn) = boot();
    insert_doc(&conn, "d1");

    conn.execute(
        "INSERT INTO chunks (id, chunk_id, doc_id, seq, text, embed_text,
                             token_count, embedding_model, content_hash)
         VALUES (1, 'c1', 'd1', 0, 'SQLite is an embedded database', 'SQLite is an embedded database',
                 5, 'bge-small-en-v1.5', 'h1')",
        [],
    )
    .unwrap();
    let hits: i64 = conn
        .query_row(
            "SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH 'sqlite'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        hits, 1,
        "INSERT must reach the BM25 index via chunks_fts_ai"
    );

    conn.execute("DELETE FROM chunks WHERE chunk_id = 'c1'", [])
        .unwrap();
    let hits: i64 = conn
        .query_row(
            "SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH 'sqlite'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        hits, 0,
        "DELETE must leave the BM25 index via chunks_fts_ad"
    );
}

#[test]
fn claim_orders_by_priority_and_reclaims_expired_leases() {
    let (_dir, conn) = boot();
    insert_doc(&conn, "d1");
    insert_doc(&conn, "d2");
    insert_doc(&conn, "d3");

    // Three claimable SCRAPE jobs: j2 urgent (lower = sooner), j1 normal, j3 gated.
    // One job row per (doc_id, stage) — §6, enforced by the schema's UNIQUE.
    control::enqueue(
        &conn,
        "j1",
        "d1",
        Stage::Scrape,
        5,
        None,
        "2026-09-06 12:00:01",
    )
    .unwrap();
    control::enqueue(
        &conn,
        "j2",
        "d2",
        Stage::Scrape,
        1,
        None,
        "2026-09-06 12:00:02",
    )
    .unwrap();

    let now = "2026-09-06 12:05:00";
    let first = control::claim_next(&conn, Stage::Scrape, "w1", now, 60)
        .unwrap()
        .expect("claim");
    assert_eq!(
        first.job_id(),
        "j2",
        "priority 1 beats priority 5 despite later created_at"
    );
    assert_eq!(
        first.attempts(),
        0,
        "fresh claims do not increment attempts (§6)"
    );

    let second = control::claim_next(&conn, Stage::Scrape, "w1", now, 60)
        .unwrap()
        .expect("claim");
    assert_eq!(second.job_id(), "j1");
    assert!(
        control::claim_next(&conn, Stage::Scrape, "w1", now, 60)
            .unwrap()
            .is_none()
    );

    // Expired lease → reclaimable, attempts++ (ended execution), last_error stamped (§6).
    conn.execute(
        "UPDATE jobs SET status = 'RUNNING', lease_owner = 'w0',
                lease_expires_at = '2026-09-06 12:00:30'
          WHERE job_id = 'j2'",
        [],
    )
    .unwrap();
    let reclaimed = control::claim_next(&conn, Stage::Scrape, "w1", now, 60)
        .unwrap()
        .expect("reclaim");
    assert_eq!(reclaimed.job_id(), "j2");
    assert_eq!(reclaimed.attempts(), 1);
    let last_error: String = conn
        .query_row("SELECT last_error FROM jobs WHERE job_id = 'j2'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(last_error, "lease expired");

    // Backoff gate: a PENDING job due in the future is not claimable (§6).
    control::enqueue(
        &conn,
        "j3",
        "d3",
        Stage::Scrape,
        1,
        None,
        "2026-09-06 12:05:01",
    )
    .unwrap();
    conn.execute(
        "UPDATE jobs SET next_attempt_at = '2026-09-06 12:10:00' WHERE job_id = 'j3'",
        [],
    )
    .unwrap();
    assert!(
        control::claim_next(&conn, Stage::Scrape, "w1", now, 60)
            .unwrap()
            .is_none()
    );

    // Claims are stage-scoped (§6): the CLEAN queue stays empty.
    assert!(
        control::claim_next(&conn, Stage::Clean, "w1", now, 60)
            .unwrap()
            .is_none()
    );
}

#[test]
fn dead_job_fails_its_document() {
    let (_dir, conn) = boot();
    insert_doc(&conn, "d1");
    control::enqueue(
        &conn,
        "j1",
        "d1",
        Stage::Extract,
        5,
        None,
        "2026-09-06 12:00:00",
    )
    .unwrap();

    control::claim_next(&conn, Stage::Extract, "w1", "2026-09-06 12:00:10", 60).unwrap();
    control::dead(&conn, "j1", "d1", "boom", "2026-09-06 12:00:20").unwrap();

    let (job_status, doc_status): (String, String) = conn
        .query_row(
            "SELECT j.status, d.status FROM jobs j JOIN documents d USING (doc_id)
              WHERE j.job_id = 'j1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(job_status, "DEAD");
    assert_eq!(
        doc_status, "FAILED",
        "§6 terminal mapping: DEAD ⇒ document FAILED"
    );
    let error: String = conn
        .query_row("SELECT error FROM documents WHERE doc_id = 'd1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(error, "boom");
}

#[test]
fn audit_records_transitions() {
    let (_dir, conn) = boot();
    insert_doc(&conn, "d1");
    control::record_event(&conn, Some("d1"), Some("j1"), Some("SCRAPE"), "DONE", None).unwrap();
    control::record_event(
        &conn,
        Some("d1"),
        Some("j1"),
        Some("SCRAPE"),
        "PANIC",
        Some("oops"),
    )
    .unwrap();

    let rows: i64 = conn
        .query_row(
            "SELECT count(*) FROM stage_events WHERE doc_id = 'd1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 2, "§1.2.6: every transition and error is auditable");
}
