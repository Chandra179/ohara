//! The job queue: the atomic lease-based claim (§6) and classification-driven
//! transitions, including stage chaining. This lives inside the `SQLite` module
//! because claiming and completing must be atomic against the single-writer WAL
//! database (§1.2, §6).

use rusqlite::Connection;

use super::db::{DbError, now_plus};
use super::models::{ClaimedJob, Completion, Stage, StageEvent, new_id};

/// The §6 atomic claim, verbatim: one `UPDATE` over a correlated single-row sub-select.
/// `attempts` increments only on expired-lease reclaim, because attempts count *ended
/// executions*, not claims — a clean crash mid-lease is punished exactly once, at
/// reclaim. `ORDER BY priority, created_at, job_id` is the §2.1 queue ordering (the
/// uuidv7 tiebreak makes it total).
const CLAIM_SQL: &str = "
UPDATE jobs
   SET status = 'RUNNING',
       lease_owner = ?1,
       lease_expires_at = ?2,
       attempts   = attempts + (CASE WHEN status = 'RUNNING' THEN 1 ELSE 0 END),
       last_error = CASE WHEN status = 'RUNNING' THEN 'lease expired'
                         ELSE last_error END,
       updated_at = ?3
 WHERE job_id = (
     SELECT jobs.job_id FROM jobs
      JOIN documents ON documents.doc_id = jobs.doc_id
      LEFT JOIN deletions ON deletions.doc_id = jobs.doc_id
      WHERE jobs.stage = ?4
        AND documents.status <> 'ARCHIVED'
        AND deletions.doc_id IS NULL
        AND ( (jobs.status = 'PENDING'
               AND (jobs.next_attempt_at IS NULL OR jobs.next_attempt_at <= ?3))
           OR (jobs.status = 'RUNNING' AND jobs.lease_expires_at < ?3) )
      ORDER BY jobs.priority, jobs.created_at, jobs.job_id
      LIMIT 1)
RETURNING job_id, doc_id, attempts, max_attempts, params, priority;
";

/// The §6 stage-chaining insert. The `UNIQUE(doc_id, stage)` constraint makes the
/// §6 one-row-per-(doc, stage) invariant enforceable; the `DO UPDATE` arm *resurrects*
/// terminal rows (`DEAD`/`DONE` — a replay of that stage) and never disturbs live
/// `PENDING`/`RUNNING` work. Chained jobs inherit the completing job's priority.
const CHAIN_SQL: &str = "
INSERT INTO jobs (job_id, doc_id, stage, status, priority, created_at, updated_at)
VALUES (?1, ?2, ?3, 'PENDING', ?4, ?5, ?5)
ON CONFLICT (doc_id, stage) DO UPDATE SET
    status = 'PENDING',
    attempts = 0,
    next_attempt_at = NULL,
    lease_owner = NULL,
    lease_expires_at = NULL,
    last_error = NULL,
    priority = excluded.priority,
    updated_at = excluded.updated_at
  WHERE jobs.status IN ('DEAD', 'DONE');
";

/// Atomically claims the next runnable job for `stage` — a fresh `PENDING` job past
/// its backoff gate, or a `RUNNING` job whose lease has expired — under the §6
/// ordering (`priority`, `created_at`, uuidv7 tiebreak).
///
/// # Errors
/// [`DbError::Sqlite`] on statement failure, or [`DbError::Timestamp`] if `now_stamp`
/// does not follow the §5 format.
pub fn claim_next(
    conn: &Connection,
    stage: Stage,
    worker: &str,
    now_stamp: &str,
    lease_secs: u64,
) -> Result<Option<ClaimedJob>, DbError> {
    let lease_expires_at = now_plus(now_stamp, lease_secs)?;
    let mut stmt = conn.prepare(CLAIM_SQL)?;
    let mut rows = stmt.query(rusqlite::params![
        worker,
        lease_expires_at,
        now_stamp,
        stage.as_str()
    ])?;
    match rows.next()? {
        Some(row) => Ok(Some(ClaimedJob {
            job_id: row.get(0)?,
            doc_id: row.get(1)?,
            attempts: row.get(2)?,
            max_attempts: row.get(3)?,
            params: row.get(4)?,
            priority: row.get(5)?,
        })),
        None => Ok(None),
    }
}

/// Marks a claimed job `DONE` and applies the [`Completion`] decision in one
/// transaction (§6 stage chaining): the document milestone advances and, for
/// [`Completion::Chain`], the successor stage's `PENDING` job is inserted here —
/// a crash between milestone and chaining is therefore impossible. Advancing the
/// milestone also clears the document's `error` (the last failure is stale once a
/// later stage completes).
///
/// # Errors
/// [`DbError::Sqlite`] on statement failure; the transaction rolls back, leaving
/// the job `RUNNING` (its lease makes it reclaimable).
pub fn complete(
    conn: &Connection,
    stage: Stage,
    job: &ClaimedJob,
    completion: Completion,
    now_stamp: &str,
) -> Result<(), DbError> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE jobs
            SET status = 'DONE', lease_owner = NULL, lease_expires_at = NULL,
                next_attempt_at = NULL, updated_at = ?2
          WHERE job_id = ?1",
        rusqlite::params![job.job_id(), now_stamp],
    )?;
    if matches!(completion, Completion::Chain | Completion::Milestone) {
        tx.execute(
            "UPDATE documents
                SET status = ?2, last_processed_at = ?3, error = NULL
              WHERE doc_id = ?1",
            rusqlite::params![job.doc_id(), stage.milestone(), now_stamp],
        )?;
    }
    if let (Completion::Chain, Some(next)) = (completion, stage.successor()) {
        tx.execute(
            CHAIN_SQL,
            rusqlite::params![
                new_id(),
                job.doc_id(),
                next.as_str(),
                job.priority(),
                now_stamp
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Sends a transiently failed job back to `PENDING` with backoff (§6): `attempts++`,
/// `next_attempt_at = due`. The claim predicate makes the backoff real — without it a
/// `PENDING` retry would be immediately reclaimable.
///
/// # Errors
/// [`DbError::Sqlite`] on statement failure.
pub fn retry(
    conn: &Connection,
    job_id: &str,
    due: &str,
    error: &str,
    now_stamp: &str,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE jobs
            SET status = 'PENDING', attempts = attempts + 1, next_attempt_at = ?2,
                last_error = ?3, lease_owner = NULL, lease_expires_at = NULL,
                updated_at = ?4
          WHERE job_id = ?1",
        rusqlite::params![job_id, due, error, now_stamp],
    )?;
    Ok(())
}

/// Marks a job `DEAD` (attempts exhausted or a permanent error) and, per the §6
/// terminal mapping, sets the document to `FAILED` with the error recorded.
///
/// # Errors
/// [`DbError::Sqlite`] on statement failure.
pub fn dead(
    conn: &Connection,
    job_id: &str,
    doc_id: &str,
    error: &str,
    now_stamp: &str,
) -> Result<(), DbError> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE jobs
            SET status = 'DEAD', last_error = ?2, lease_owner = NULL,
                lease_expires_at = NULL, updated_at = ?3
          WHERE job_id = ?1",
        rusqlite::params![job_id, error, now_stamp],
    )?;
    tx.execute(
        "UPDATE documents
            SET status = 'FAILED', error = ?2, last_processed_at = ?3
          WHERE doc_id = ?1",
        rusqlite::params![doc_id, error, now_stamp],
    )?;
    tx.commit()?;
    Ok(())
}

/// Operator recovery for a failed document (§6: `ohara requeue --doc <id>`): resets
/// the document's non-`DONE` jobs to `PENDING` with a fresh attempt budget
/// (`attempts = 0`). `DONE` stages are left alone — re-running completed work is the
/// re-crawl flow's (§7.5) decision, not the operator's repair. Returns the number of
/// jobs reset.
///
/// # Errors
/// [`DbError::Sqlite`] on statement failure.
pub fn requeue(conn: &Connection, doc_id: &str, now_stamp: &str) -> Result<usize, DbError> {
    let reset = conn.execute(
        "UPDATE jobs
            SET status = 'PENDING', attempts = 0, next_attempt_at = NULL,
                last_error = NULL, lease_owner = NULL, lease_expires_at = NULL,
                updated_at = ?2
          WHERE doc_id = ?1 AND status IN ('DEAD', 'RUNNING')",
        rusqlite::params![doc_id, now_stamp],
    )?;
    Ok(reset)
}

/// Appends an audit row to `stage_events` (§1.2.6: every transition and every error).
/// `outcome` is `DONE | RETRY | DEAD | FATAL | PANIC | SKIP` (§5); the table has no FK — the audit
/// trail outlives deleted rows.
///
/// # Errors
/// [`DbError::Sqlite`] on statement failure.
pub fn record_event(
    conn: &Connection,
    doc_id: Option<&str>,
    job_id: Option<&str>,
    stage: Option<&str>,
    outcome: &str,
    detail: Option<&str>,
) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO stage_events (doc_id, job_id, stage, outcome, detail)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![doc_id, job_id, stage, outcome, detail],
    )?;
    Ok(())
}

/// Reads the retained audit trail for one document in insertion order (§13).
/// The query stays in the control plane so callers never receive a SQLite
/// connection or vendor row type.
///
/// # Errors
/// [`DbError::Sqlite`] on statement or row decoding failure.
pub fn events_for_doc(conn: &Connection, doc_id: &str) -> Result<Vec<StageEvent>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT event_id, doc_id, job_id, stage, outcome, detail, ts
           FROM stage_events
          WHERE doc_id = ?1
          ORDER BY event_id",
    )?;
    let rows = stmt.query_map([doc_id], |row| {
        Ok(StageEvent {
            event_id: row.get(0)?,
            doc_id: row.get(1)?,
            job_id: row.get(2)?,
            stage: row.get(3)?,
            outcome: row.get(4)?,
            detail: row.get(5)?,
            ts: row.get(6)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
}

/// Enqueues a `PENDING` job (priority per §6; `params` is JSON or `NULL`).
///
/// # Errors
/// [`DbError::Sqlite`] on statement failure (e.g. unknown document, or a `UNIQUE`
/// violation of the §6 one-row-per-(doc, stage) invariant).
pub fn enqueue(
    conn: &Connection,
    job_id: &str,
    doc_id: &str,
    stage: Stage,
    priority: i64,
    params: Option<&str>,
    now_stamp: &str,
) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO jobs (job_id, doc_id, stage, status, priority, params, created_at, updated_at)
         VALUES (?1, ?2, ?3, 'PENDING', ?4, ?5, ?6, ?6)",
        rusqlite::params![job_id, doc_id, stage.as_str(), priority, params, now_stamp],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // §10: tests unwrap freely

    use super::super::documents;
    use super::super::testing::{boot_raw as boot, seed_doc_raw as seed_doc};
    use super::*;

    const NOW: &str = "2026-09-06 12:05:00";

    fn claim(conn: &rusqlite::Connection, stage: Stage) -> ClaimedJob {
        claim_next(conn, stage, "w1", NOW, 60)
            .unwrap()
            .expect("claimable job")
    }

    fn job_status(conn: &rusqlite::Connection, doc_id: &str, stage: Stage) -> Option<String> {
        conn.query_row(
            "SELECT status FROM jobs WHERE doc_id = ?1 AND stage = ?2",
            rusqlite::params![doc_id, stage.as_str()],
            |r| r.get(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
        .unwrap()
    }

    fn doc_status(conn: &rusqlite::Connection, doc_id: &str) -> String {
        conn.query_row(
            "SELECT status FROM documents WHERE doc_id = ?1",
            [doc_id],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn complete_chain_advances_milestone_and_enqueues_successor() {
        let conn = boot();
        let doc_id = seed_doc(&conn, "d1");
        let job = claim(&conn, Stage::Scrape);

        complete(&conn, Stage::Scrape, &job, Completion::Chain, NOW).unwrap();

        assert_eq!(
            job_status(&conn, &doc_id, Stage::Scrape).as_deref(),
            Some("DONE")
        );
        assert_eq!(
            doc_status(&conn, &doc_id),
            "SCRAPED",
            "§5: completed milestone"
        );
        let (status, priority): (String, i64) = conn
            .query_row(
                "SELECT status, priority FROM jobs WHERE doc_id = ?1 AND stage = 'CLEAN'",
                [&doc_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            status, "PENDING",
            "§6: the successor job is chained in the same tx"
        );
        assert_eq!(
            priority, 5,
            "chained jobs inherit the completing job's priority"
        );
    }

    #[test]
    fn complete_chain_never_disturbs_a_live_successor_job() {
        let conn = boot();
        let doc_id = seed_doc(&conn, "d1");
        enqueue(&conn, "pre-existing", &doc_id, Stage::Clean, 9, None, NOW).unwrap();
        let job = claim(&conn, Stage::Scrape);

        complete(&conn, Stage::Scrape, &job, Completion::Chain, NOW).unwrap();

        let (status, priority, attempts): (String, i64, i64) = conn
            .query_row(
                "SELECT status, priority, attempts FROM jobs WHERE job_id = 'pre-existing'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (status.as_str(), priority, attempts),
            ("PENDING", 9, 0),
            "chaining resurrects terminal rows only (§6 one-row-per-doc+stage)"
        );
    }

    #[test]
    fn complete_chain_resurrects_a_dead_successor() {
        let conn = boot();
        let doc_id = seed_doc(&conn, "d1");
        enqueue(&conn, "dead-clean", &doc_id, Stage::Clean, 5, None, NOW).unwrap();
        conn.execute(
            "UPDATE jobs SET status = 'DEAD', attempts = 5, last_error = 'boom'
              WHERE job_id = 'dead-clean'",
            [],
        )
        .unwrap();
        let job = claim(&conn, Stage::Scrape);

        complete(&conn, Stage::Scrape, &job, Completion::Chain, NOW).unwrap();

        let (status, attempts, last_error): (String, i64, Option<String>) = conn
            .query_row(
                "SELECT status, attempts, last_error FROM jobs WHERE job_id = 'dead-clean'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "PENDING");
        assert_eq!(attempts, 0, "replay starts with a fresh attempt budget");
        assert_eq!(last_error, None::<String>);
    }

    #[test]
    fn complete_milestone_ends_the_chain_when_the_graph_is_off() {
        let conn = boot();
        let doc_id = seed_doc(&conn, "d1");
        // Drive to VECTORIZED: SCRAPE → CLEAN → VECTORIZE.
        for stage in [Stage::Scrape, Stage::Clean] {
            let job = claim(&conn, stage);
            complete(&conn, stage, &job, Completion::Chain, NOW).unwrap();
        }
        let job = claim(&conn, Stage::Vectorize);

        complete(&conn, Stage::Vectorize, &job, Completion::Milestone, NOW).unwrap();

        assert_eq!(
            doc_status(&conn, &doc_id),
            "VECTORIZED",
            "§6: docs stay VECTORIZED"
        );
        assert_eq!(
            job_status(&conn, &doc_id, Stage::Extract),
            None,
            "no EXTRACT job when the graph is disabled"
        );
    }

    #[test]
    fn complete_done_records_a_terminal_domain_outcome_only() {
        let conn = boot();
        let doc_id = seed_doc(&conn, "d1");
        documents::mark_quality_rejected(&conn, &doc_id, "word count < 50", NOW).unwrap();
        let job = claim(&conn, Stage::Scrape);

        complete(&conn, Stage::Scrape, &job, Completion::Done, NOW).unwrap();

        assert_eq!(
            job_status(&conn, &doc_id, Stage::Scrape).as_deref(),
            Some("DONE")
        );
        assert_eq!(
            doc_status(&conn, &doc_id),
            "FAILED_QUALITY",
            "the stage's own outcome stands; complete must not overwrite it"
        );
        assert_eq!(
            job_status(&conn, &doc_id, Stage::Clean),
            None,
            "a rejected document chains nothing"
        );
    }

    #[test]
    fn complete_chain_clears_a_stale_error() {
        let conn = boot();
        let doc_id = seed_doc(&conn, "d1");
        conn.execute(
            "UPDATE documents SET error = 'old failure' WHERE doc_id = ?1",
            [&doc_id],
        )
        .unwrap();
        let job = claim(&conn, Stage::Scrape);

        complete(&conn, Stage::Scrape, &job, Completion::Chain, NOW).unwrap();

        let error: Option<String> = conn
            .query_row(
                "SELECT error FROM documents WHERE doc_id = ?1",
                [&doc_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            error, None,
            "a completed milestone invalidates the last failure"
        );
    }

    #[test]
    fn requeue_resets_dead_and_running_jobs_with_a_fresh_budget() {
        let conn = boot();
        let doc_id = seed_doc(&conn, "d1");
        let job = claim(&conn, Stage::Scrape);
        complete(&conn, Stage::Scrape, &job, Completion::Chain, NOW).unwrap();
        let clean = claim(&conn, Stage::Clean);
        dead(&conn, clean.job_id(), &doc_id, "boom", NOW).unwrap();

        let reset = requeue(&conn, &doc_id, NOW).unwrap();

        assert_eq!(reset, 1);
        assert_eq!(
            job_status(&conn, &doc_id, Stage::Scrape).as_deref(),
            Some("DONE"),
            "completed stages are not re-run by requeue (§6)"
        );
        let (status, attempts, last_error): (String, i64, Option<String>) = conn
            .query_row(
                "SELECT status, attempts, last_error FROM jobs WHERE job_id = ?1",
                [clean.job_id()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "PENDING");
        assert_eq!(attempts, 0, "§6: requeue grants a fresh attempt budget");
        assert_eq!(last_error, None);
        assert_eq!(
            doc_status(&conn, &doc_id),
            "FAILED",
            "milestone untouched until re-run"
        );
    }
}
