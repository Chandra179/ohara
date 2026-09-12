//! Entity-resolution review candidates.

use rusqlite::Connection;

use super::super::db::DbError;

/// One pending cross-document entity-resolution review candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct ErReview {
    /// Review row id.
    pub id: i64,
    /// First entity captured by the detector.
    pub entity_a: String,
    /// Second entity captured by the detector.
    pub entity_b: String,
    /// Similarity score that caused the review.
    pub score: Option<f64>,
}

/// Reads the pending cross-document merge queue.
///
/// # Errors
/// [`DbError`] on statement failure.
pub fn pending_er_reviews(conn: &Connection) -> Result<Vec<ErReview>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT id, entity_a, entity_b, score
           FROM er_review
          WHERE status = 'PENDING'
          ORDER BY created_at, id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(ErReview {
            id: row.get(0)?,
            entity_a: row.get(1)?,
            entity_b: row.get(2)?,
            score: row.get(3)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Files a cross-document merge candidate (§7.8, §8 Stage 4.3): two entity ids
/// that similarity matching could not separate. Duplicate `PENDING` rows for the
/// same unordered pair are skipped — the queue holds one decision per pair.
/// Returns whether a new row was filed.
///
/// # Errors
/// [`DbError`] on statement failure.
pub fn er_review_candidate(
    conn: &Connection,
    entity_a: &str,
    entity_b: &str,
    score: f64,
) -> Result<bool, DbError> {
    if entity_a == entity_b {
        return Ok(false);
    }
    let mut stmt = conn.prepare(
        "SELECT count(*) FROM er_review
          WHERE status = 'PENDING'
            AND ((entity_a = ?1 AND entity_b = ?2) OR (entity_a = ?2 AND entity_b = ?1))",
    )?;
    let pending: i64 = stmt.query_row(rusqlite::params![entity_a, entity_b], |row| row.get(0))?;
    if pending > 0 {
        return Ok(false);
    }
    let inserted = conn.execute(
        "INSERT INTO er_review (entity_a, entity_b, score, status)
         VALUES (?1, ?2, ?3, 'PENDING')",
        rusqlite::params![entity_a, entity_b, score],
    )?;
    Ok(inserted == 1)
}

/// Closes a review whose two ids already resolve to the same merged root.
///
/// # Errors
/// [`DbError`] on statement failure.
pub fn mark_er_review_merged(conn: &Connection, review_id: i64) -> Result<(), DbError> {
    conn.execute(
        "UPDATE er_review SET status = 'MERGED' WHERE id = ?1 AND status = 'PENDING'",
        [review_id],
    )?;
    Ok(())
}
