//! The job queue: the atomic lease-based claim (§6) and classification-driven
//! transitions. This lives inside the `SQLite` module because claiming must be one
//! atomic statement against the single-writer WAL database (§1.2, §6).

use rusqlite::Connection;

use super::db::{DbError, now_plus};
use super::models::{ClaimedJob, Stage};

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
     SELECT job_id FROM jobs
      WHERE stage = ?4
        AND ( (status = 'PENDING'
               AND (next_attempt_at IS NULL OR next_attempt_at <= ?3))
           OR (status = 'RUNNING' AND lease_expires_at < ?3) )
      ORDER BY priority, created_at, job_id
      LIMIT 1)
RETURNING job_id, doc_id, attempts, max_attempts, params;
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
        })),
        None => Ok(None),
    }
}

/// Marks a claimed job `DONE` and clears its lease. Stage chaining — inserting the
/// next stage's `PENDING` job in this same transaction (§6) — arrives with the
/// control-store build step (§15 step 2).
///
/// # Errors
/// [`DbError::Sqlite`] on statement failure.
pub fn complete(conn: &Connection, job_id: &str, now_stamp: &str) -> Result<(), DbError> {
    conn.execute(
        "UPDATE jobs
            SET status = 'DONE', lease_owner = NULL, lease_expires_at = NULL,
                next_attempt_at = NULL, updated_at = ?2
          WHERE job_id = ?1",
        rusqlite::params![job_id, now_stamp],
    )?;
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

/// Appends an audit row to `stage_events` (§1.2.6: every transition and every error).
/// `outcome` is `DONE | RETRY | DEAD | PANIC` (§5); the table has no FK — the audit
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

/// Enqueues a `PENDING` job (priority per §6; `params` is JSON or `NULL`).
///
/// # Errors
/// [`DbError::Sqlite`] on statement failure (e.g. unknown document).
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
