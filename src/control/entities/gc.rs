//! Entity garbage-collection candidate lifecycle.

use rusqlite::Connection;

use super::super::db::DbError;

/// An unmerged entity observed without any knowledge-plane mentions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityGcCandidate {
    /// Stable entity id awaiting the grace period.
    pub entity_id: String,
    /// First timestamp at which the entity was observed with zero mentions.
    pub zero_since: String,
}

/// Lists entities that are safe to consider for garbage collection. Entities
/// referenced by merge audit rows remain durable so the audit stays resolvable.
///
/// # Errors
/// [`DbError`] on statement failure.
pub fn entity_ids_for_gc(conn: &Connection) -> Result<Vec<String>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT e.entity_id
           FROM entities e
          WHERE NOT EXISTS (
                    SELECT 1 FROM entity_merges m
                     WHERE m.loser_id = e.entity_id OR m.winner_id = e.entity_id
                )
          ORDER BY e.entity_id",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Records the first zero-mention observation for an entity. The original
/// timestamp is preserved across repeated sweeps, making the grace period
/// stable and replay-safe. Returns whether a candidate was newly inserted.
///
/// # Errors
/// [`DbError`] on statement failure.
pub fn record_entity_gc_candidate(
    conn: &Connection,
    entity_id: &str,
    zero_since: &str,
) -> Result<bool, DbError> {
    let inserted = conn.execute(
        "INSERT INTO entity_gc_candidates (entity_id, zero_since)
         SELECT ?1, ?2
          WHERE EXISTS (SELECT 1 FROM entities WHERE entity_id = ?1)
         ON CONFLICT(entity_id) DO NOTHING",
        rusqlite::params![entity_id, zero_since],
    )?;
    Ok(inserted == 1)
}

/// Cancels a zero-mention candidate after a mention reappears.
///
/// # Errors
/// [`DbError`] on statement failure.
pub fn cancel_entity_gc_candidate(conn: &Connection, entity_id: &str) -> Result<bool, DbError> {
    Ok(conn.execute(
        "DELETE FROM entity_gc_candidates WHERE entity_id = ?1",
        [entity_id],
    )? == 1)
}

/// Lists candidates whose grace period has elapsed. The caller must recheck
/// mentions immediately before deleting because the graph is a separate store.
///
/// # Errors
/// [`DbError`] on statement failure.
pub fn due_entity_gc_candidates(
    conn: &Connection,
    cutoff_stamp: &str,
) -> Result<Vec<EntityGcCandidate>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT entity_id, zero_since
           FROM entity_gc_candidates
          WHERE zero_since <= ?1
          ORDER BY zero_since, entity_id",
    )?;
    let rows = stmt.query_map([cutoff_stamp], |row| {
        Ok(EntityGcCandidate {
            entity_id: row.get(0)?,
            zero_since: row.get(1)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Removes an unmerged entity's control-plane record after its knowledge-plane
/// node and vector have been deleted. Merge audit references are never removed;
/// an entity referenced by that audit is retained and returns `false`.
///
/// # Errors
/// [`DbError`] if the transaction fails.
pub fn execute_entity_gc(conn: &Connection, entity_id: &str) -> Result<bool, DbError> {
    let tx = conn.unchecked_transaction()?;
    let referenced: bool = tx.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM entity_merges
              WHERE loser_id = ?1 OR winner_id = ?1
         )",
        [entity_id],
        |row| row.get(0),
    )?;
    if referenced {
        tx.execute(
            "DELETE FROM entity_gc_candidates WHERE entity_id = ?1",
            [entity_id],
        )?;
        tx.commit()?;
        return Ok(false);
    }

    tx.execute(
        "UPDATE er_review
            SET status = 'REJECTED'
          WHERE status = 'PENDING' AND (entity_a = ?1 OR entity_b = ?1)",
        [entity_id],
    )?;
    tx.execute(
        "DELETE FROM entity_aliases WHERE entity_id = ?1",
        [entity_id],
    )?;
    let deleted = tx.execute("DELETE FROM entities WHERE entity_id = ?1", [entity_id])?;
    tx.commit()?;
    Ok(deleted == 1)
}
