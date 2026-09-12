//! Offline entity-merge records and resolution.

use rusqlite::{Connection, OptionalExtension};

use super::super::db::DbError;

/// The stable control-plane metadata for one entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityDetails {
    /// Stable uuidv7 surrogate.
    pub entity_id: String,
    /// Canonical display name.
    pub canonical_name: String,
    /// Closed ontology supertype.
    pub entity_type: String,
    /// Creation timestamp used as the deterministic merge tie-breaker.
    pub created_at: String,
}

/// One recorded offline entity merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityMerge {
    /// Entity folded away in the knowledge plane.
    pub loser_id: String,
    /// Entity retained as the canonical graph node.
    pub winner_id: String,
}

/// Reads one entity's merge-decision metadata.
///
/// # Errors
/// [`DbError`] on statement failure.
pub fn entity_details(
    conn: &Connection,
    entity_id: &str,
) -> Result<Option<EntityDetails>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT entity_id, canonical_name, entity_type, created_at
           FROM entities
          WHERE entity_id = ?1",
    )?;
    let mut rows = stmt.query([entity_id])?;
    match rows.next()? {
        Some(row) => Ok(Some(EntityDetails {
            entity_id: row.get(0)?,
            canonical_name: row.get(1)?,
            entity_type: row.get(2)?,
            created_at: row.get(3)?,
        })),
        None => Ok(None),
    }
}

/// Resolves an entity through the recorded merge chain to its current root.
///
/// # Errors
/// [`DbError`] on statement failure or if the merge audit contains a cycle.
pub fn resolve_entity(conn: &Connection, entity_id: &str) -> Result<String, DbError> {
    if entity_details(conn, entity_id)?.is_none() {
        return Err(DbError::EntityNotFound {
            entity_id: entity_id.to_string(),
        });
    }
    let mut current = entity_id.to_string();
    let mut seen = std::collections::HashSet::new();
    loop {
        if !seen.insert(current.clone()) {
            return Err(DbError::MergeCycle { entity_id: current });
        }
        let next = conn
            .query_row(
                "SELECT winner_id FROM entity_merges WHERE loser_id = ?1",
                [&current],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(next) = next else {
            return Ok(current);
        };
        current = next;
    }
}

/// Reads every recorded merge for the knowledge-plane repair sweep.
///
/// # Errors
/// [`DbError`] on statement failure.
pub fn entity_merges(conn: &Connection) -> Result<Vec<EntityMerge>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT loser_id, winner_id
           FROM entity_merges
          ORDER BY merged_at, loser_id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(EntityMerge {
            loser_id: row.get(0)?,
            winner_id: row.get(1)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Records the `SQLite` half of an offline entity merge.
///
/// Aliases are remapped before the audit row is inserted. If the winner already
/// owns one of the loser's aliases, the loser mapping is discarded instead of
/// violating the typed-alias primary key. The operation is idempotent and also
/// closes the exact pending review pair.
///
/// # Errors
/// [`DbError`] on statement failure or when the entities do not exist, have
/// different supertypes, or would create a merge cycle.
pub fn record_entity_merge(
    conn: &Connection,
    loser_id: &str,
    winner_id: &str,
    reason: &str,
) -> Result<bool, DbError> {
    if loser_id == winner_id {
        return Err(DbError::MergeSelf {
            entity_id: loser_id.to_string(),
        });
    }
    let loser = entity_details(conn, loser_id)?.ok_or_else(|| DbError::EntityNotFound {
        entity_id: loser_id.to_string(),
    })?;
    let winner = entity_details(conn, winner_id)?.ok_or_else(|| DbError::EntityNotFound {
        entity_id: winner_id.to_string(),
    })?;
    if loser.entity_type != winner.entity_type {
        return Err(DbError::EntityTypeMismatch {
            loser_id: loser_id.to_string(),
            loser_type: loser.entity_type,
            winner_id: winner_id.to_string(),
            winner_type: winner.entity_type,
        });
    }
    if resolve_entity(conn, winner_id)? == loser_id {
        return Err(DbError::MergeCycle {
            entity_id: loser_id.to_string(),
        });
    }

    let tx = conn.unchecked_transaction()?;
    let existing = tx
        .query_row(
            "SELECT winner_id FROM entity_merges WHERE loser_id = ?1",
            [loser_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(existing_winner) = existing {
        if existing_winner != winner_id {
            return Err(DbError::MergeConflict {
                loser_id: loser_id.to_string(),
                existing_winner,
                requested_winner: winner_id.to_string(),
            });
        }
        tx.execute(
            "UPDATE er_review
                SET status = 'MERGED'
              WHERE status = 'PENDING'
                AND ((entity_a = ?1 AND entity_b = ?2)
                  OR (entity_a = ?2 AND entity_b = ?1))",
            rusqlite::params![loser_id, winner_id],
        )?;
        tx.commit()?;
        return Ok(false);
    }

    tx.execute(
        "DELETE FROM entity_aliases
          WHERE entity_id = ?1
            AND EXISTS (
                SELECT 1 FROM entity_aliases winner_alias
                 WHERE winner_alias.entity_id = ?2
                   AND winner_alias.alias = entity_aliases.alias
                   AND winner_alias.entity_type = entity_aliases.entity_type
            )",
        rusqlite::params![loser_id, winner_id],
    )?;
    tx.execute(
        "UPDATE entity_aliases
            SET entity_id = ?1, source = 'merge'
          WHERE entity_id = ?2",
        rusqlite::params![winner_id, loser_id],
    )?;
    tx.execute(
        "INSERT INTO entity_merges (loser_id, winner_id, reason)
         VALUES (?1, ?2, ?3)",
        rusqlite::params![loser_id, winner_id, reason],
    )?;
    tx.execute(
        "UPDATE er_review
            SET status = 'MERGED'
          WHERE status = 'PENDING'
            AND ((entity_a = ?1 AND entity_b = ?2)
              OR (entity_a = ?2 AND entity_b = ?1))",
        rusqlite::params![loser_id, winner_id],
    )?;
    tx.commit()?;
    Ok(true)
}
