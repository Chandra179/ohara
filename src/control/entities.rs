//! Stage 4's control-plane tables (§5): `triplets` (the extraction checkpoint
//! and cost cache, §7.7), `entities` / `entity_aliases` (the canonical entity
//! registry, §8 Stage 4), and `er_review` (cross-doc merge candidates awaiting a
//! decision, §7.8). The knowledge plane's graph is rebuilt from these rows
//! (§7.9) — `SQLite` is the system of record here.

use rusqlite::{Connection, OptionalExtension};

use super::db::DbError;
use super::documents::ChunkText;
use super::models::new_id;

/// A triplet staged for storage (§8 Stage 4): per-chunk evidence keyed by
/// surface strings — identity is the content hash `triplet_id`, not the row
/// contents. Plain data record (`CODE_GUIDE` §6 DTO exception); the §5 CHECK
/// constraints guarantee validity.
#[derive(Debug, Clone)]
pub struct NewTriplet {
    /// `sha256(chunk_id || subject || predicate || object)` (§3).
    pub triplet_id: String,
    /// The chunk this evidence came from.
    pub chunk_id: String,
    /// Surface string — evidence, not identity (§8 Stage 4).
    pub subject: String,
    /// Supertype discriminator (§5 CHECK).
    pub subject_type: String,
    /// Relation — the §5 CHECK's closed predicate set.
    pub predicate: String,
    /// Surface string — evidence, not identity.
    pub object: String,
    /// Supertype discriminator (§5 CHECK).
    pub object_type: String,
    /// JSON (`occurred_on`, `as_of`, …) or `NULL`.
    pub properties: Option<String>,
    /// Extractor model/prompt version (§11 cost guard).
    pub model: String,
}

/// Stages `triplets` rows: each insert is an idempotent no-op when the row
/// already exists (immutable cost-cache rows, §7.1 replay — `DO NOTHING`, never
/// `REPLACE`, which would churn the row and lose `extracted_at`). Returns only
/// the triplets that were actually newly stored — those are the ones whose
/// graph merge the caller must perform (§7.2 intent-before-write: the cache
/// commits before the knowledge plane is touched).
///
/// # Errors
/// [`DbError`] on statement failure (the transaction rolls back).
pub fn stage_triplets(
    conn: &Connection,
    triplets: Vec<NewTriplet>,
) -> Result<Vec<NewTriplet>, DbError> {
    let tx = conn.unchecked_transaction()?;
    let mut fresh = Vec::with_capacity(triplets.len());
    {
        let mut stmt = tx.prepare(
            "INSERT INTO triplets
                 (triplet_id, chunk_id, subject, subject_type, predicate, object,
                  object_type, properties, model)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(triplet_id) DO NOTHING",
        )?;
        for triplet in triplets {
            let inserted = stmt.execute(rusqlite::params![
                triplet.triplet_id,
                triplet.chunk_id,
                triplet.subject,
                triplet.subject_type,
                triplet.predicate,
                triplet.object,
                triplet.object_type,
                triplet.properties,
                triplet.model,
            ])?;
            if inserted == 1 {
                fresh.push(triplet);
            }
        }
    }
    tx.commit()?;
    Ok(fresh)
}

/// The §7.7 extraction checkpoint read: chunks of `doc_id` that carry no
/// triplet yet — exactly the chunks Stage 4 must still pay for. A resume skips
/// everything else; LLM output is never paid for twice.
///
/// # Errors
/// [`DbError`] on statement failure.
pub fn chunks_without_triplets(conn: &Connection, doc_id: &str) -> Result<Vec<ChunkText>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT c.chunk_id, c.text
           FROM chunks c
          WHERE c.doc_id = ?1
            AND NOT EXISTS (SELECT 1 FROM triplets t WHERE t.chunk_id = c.chunk_id)
          ORDER BY c.seq",
    )?;
    let rows = stmt.query_map([doc_id], |row| {
        Ok(ChunkText {
            chunk_id: row.get(0)?,
            doc_id: doc_id.to_string(),
            text: row.get(1)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// The §8 Stage 4 typed alias hit: exact `(normalized alias, entity_type)` match
/// on the `entity_aliases` PK. A same-alias-different-type situation is a miss
/// by construction — the PK spans both columns (Jordan/PERSON ≠ Jordan/LOCATION).
///
/// # Errors
/// [`DbError`] on statement failure.
pub fn lookup_alias(
    conn: &Connection,
    alias: &str,
    entity_type: &str,
) -> Result<Option<String>, DbError> {
    let mut stmt =
        conn.prepare("SELECT entity_id FROM entity_aliases WHERE alias = ?1 AND entity_type = ?2")?;
    let mut rows = stmt.query(rusqlite::params![alias, entity_type])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

/// The §8 Stage 5.2 query-entity hit: every type-variant of an alias — a
/// homograph returns all its entities and disambiguation moves downstream
/// (rerank scores the chunks; graph context weighs the facts).
///
/// # Errors
/// [`DbError`] on statement failure.
pub fn lookup_alias_all_types(conn: &Connection, alias: &str) -> Result<Vec<String>, DbError> {
    let mut stmt = conn.prepare("SELECT entity_id FROM entity_aliases WHERE alias = ?1")?;
    let rows = stmt.query_map([alias], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Registers a surface form for `entity_id` (`source = 'extraction'`). On a PK
/// conflict — the alias already maps to another entity of the same type — the
/// row is left alone and the existing mapping is returned: two entities sharing
/// one alias is merge-candidate evidence, not something extraction may silently
/// rewire (§7.8).
///
/// # Errors
/// [`DbError`] on statement failure.
pub fn upsert_alias(
    conn: &Connection,
    alias: &str,
    entity_type: &str,
    entity_id: &str,
) -> Result<Option<String>, DbError> {
    let inserted = conn.execute(
        "INSERT INTO entity_aliases (alias, entity_type, entity_id, source)
         VALUES (?1, ?2, ?3, 'extraction')
         ON CONFLICT(alias, entity_type) DO NOTHING",
        rusqlite::params![alias, entity_type, entity_id],
    )?;
    if inserted == 1 {
        return Ok(None);
    }
    lookup_alias(conn, alias, entity_type)
}

/// Merges an entity into the registry on its `UNIQUE(canonical_name, entity_type)`
/// key (§8 Stage 4): the row is created under `entity_id` if absent; either way
/// the id now owning this `(name, type)` key is returned. One transaction — the
/// key lookup and the create must be atomic.
///
/// # Errors
/// [`DbError`] on statement failure.
pub fn ensure_entity(
    conn: &Connection,
    entity_id: &str,
    canonical_name: &str,
    entity_type: &str,
    subtype: Option<&str>,
) -> Result<String, DbError> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO entities (entity_id, canonical_name, entity_type, subtype)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(canonical_name, entity_type) DO NOTHING",
        rusqlite::params![entity_id, canonical_name, entity_type, subtype],
    )?;
    let id: String = tx.query_row(
        "SELECT entity_id FROM entities WHERE canonical_name = ?1 AND entity_type = ?2",
        rusqlite::params![canonical_name, entity_type],
        |row| row.get(0),
    )?;
    tx.commit()?;
    Ok(id)
}

/// One canonical name of `entity_type` — the same-supertype candidate pool for
/// normalized-name similarity (§8 Stage 4.2).
#[derive(Debug, Clone)]
pub struct NameCandidate {
    /// The candidate's surrogate id.
    pub entity_id: String,
    /// Its canonical (registry) name.
    pub canonical_name: String,
}

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

/// An unmerged entity observed without any knowledge-plane mentions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityGcCandidate {
    /// Stable entity id awaiting the grace period.
    pub entity_id: String,
    /// First timestamp at which the entity was observed with zero mentions.
    pub zero_since: String,
}

/// One recorded offline entity merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityMerge {
    /// Entity folded away in the knowledge plane.
    pub loser_id: String,
    /// Entity retained as the canonical graph node.
    pub winner_id: String,
}

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

/// The registry supertype of one entity id, if the entity still exists. Used to
/// keep embedding-based similarity matching within the supertype (§8 Stage 4.2:
/// the `EntityNames` vector collection spans types, the registry does not).
///
/// # Errors
/// [`DbError`] on statement failure.
pub fn entity_type_of(conn: &Connection, entity_id: &str) -> Result<Option<String>, DbError> {
    let mut stmt = conn.prepare("SELECT entity_type FROM entities WHERE entity_id = ?1")?;
    let mut rows = stmt.query([entity_id])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
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

/// Reads an entity's canonical name for knowledge-plane vector repair.
pub(crate) fn canonical_name(
    conn: &Connection,
    entity_id: &str,
) -> Result<Option<String>, DbError> {
    let mut stmt = conn.prepare("SELECT canonical_name FROM entities WHERE entity_id = ?1")?;
    let mut rows = stmt.query([entity_id])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

/// Every canonical name registered under `entity_type` (§8 Stage 4.2: similarity
/// matching never crosses supertypes).
///
/// # Errors
/// [`DbError`] on statement failure.
pub fn canonical_names(
    conn: &Connection,
    entity_type: &str,
) -> Result<Vec<NameCandidate>, DbError> {
    let mut stmt =
        conn.prepare("SELECT entity_id, canonical_name FROM entities WHERE entity_type = ?1")?;
    let rows = stmt.query_map([entity_type], |row| {
        Ok(NameCandidate {
            entity_id: row.get(0)?,
            canonical_name: row.get(1)?,
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

/// A stored `triplets` row (§5), read back for the §7.3 resume re-merge: a crash
/// between staging and the graph write heals by re-merging rows idempotently —
/// never by re-paying the LLM (§7.7).
#[derive(Debug, Clone)]
pub struct TripletRow {
    /// `sha256(chunk_id || subject || predicate || object)` (§3).
    pub triplet_id: String,
    /// The evidence chunk.
    pub chunk_id: String,
    /// Surface subject string.
    pub subject: String,
    /// Supertype discriminator.
    pub subject_type: String,
    /// The relation discriminator.
    pub predicate: String,
    /// Surface object string.
    pub object: String,
    /// Supertype discriminator.
    pub object_type: String,
    /// JSON (`occurred_on`, `as_of`, …) or `NULL`.
    pub properties: Option<String>,
}

/// Every triplet staged for `doc_id`, in chunk order — the §7.3 re-merge source.
///
/// # Errors
/// [`DbError`] on statement failure.
pub fn triplets_of_doc(conn: &Connection, doc_id: &str) -> Result<Vec<TripletRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT t.triplet_id, t.chunk_id, t.subject, t.subject_type, t.predicate,
                t.object, t.object_type, t.properties
           FROM triplets t
           JOIN chunks c ON c.chunk_id = t.chunk_id
          WHERE c.doc_id = ?1
          ORDER BY c.seq",
    )?;
    let rows = stmt.query_map([doc_id], |row| {
        Ok(TripletRow {
            triplet_id: row.get(0)?,
            chunk_id: row.get(1)?,
            subject: row.get(2)?,
            subject_type: row.get(3)?,
            predicate: row.get(4)?,
            object: row.get(5)?,
            object_type: row.get(6)?,
            properties: row.get(7)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Mints a uuidv7 entity id (§3: evolving first-class entities get stable
/// surrogates; looked up by `UNIQUE(canonical_name, entity_type)`).
pub(crate) fn new_entity_id() -> String {
    new_id()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::super::documents::replace_chunks;
    use super::super::testing::{boot_raw as boot, seed_doc_raw as seed_doc};
    use super::super::{NewChunkRow, Stage};
    use super::*;

    fn triplet(chunk_id: &str, subject: &str, predicate: &str, object: &str) -> NewTriplet {
        NewTriplet {
            triplet_id: format!("id-{chunk_id}-{subject}-{predicate}-{object}"),
            chunk_id: chunk_id.to_string(),
            subject: subject.to_string(),
            subject_type: "PERSON".to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            object_type: "LOCATION".to_string(),
            properties: None,
            model: "phi4-mini:latest".to_string(),
        }
    }

    fn seed_chunks(conn: &Connection, doc_id: &str, texts: &[&str]) -> Vec<String> {
        let rows: Vec<NewChunkRow> = texts
            .iter()
            .enumerate()
            .map(|(seq, text)| NewChunkRow {
                chunk_id: crate::text::sha256_hex(&format!("{doc_id}:{seq}")),
                seq: i64::try_from(seq).unwrap_or(i64::MAX),
                header_path: "h".to_string(),
                text: text.to_string(),
                embed_text: text.to_string(),
                token_count: 10,
                embedding_model: "bge-small-en-v1.5".to_string(),
                content_hash: crate::text::sha256_hex(text),
            })
            .collect();
        let ids = rows.iter().map(|r| r.chunk_id.clone()).collect();
        replace_chunks(conn, doc_id, &rows).unwrap();
        ids
    }

    #[test]
    fn stage_triplets_returns_only_fresh_rows_and_replays_are_noops() {
        let conn = boot();
        let doc_id = seed_doc(&conn, "d");
        let chunk = seed_chunks(&conn, &doc_id, &["text"])[0].clone();
        let row = triplet(&chunk, "Ada", "LOCATED_IN", "London");

        let fresh = stage_triplets(&conn, vec![row.clone()]).unwrap();
        assert_eq!(fresh.len(), 1);
        // §7.1 replay: the same row is invisible on a second pass.
        let replayed = stage_triplets(&conn, vec![row]).unwrap();
        assert!(replayed.is_empty(), "cost cache is never paid twice (§7.7)");
        // The §6 one-row invariant shape: no duplicate rows exist.
        let count: i64 = conn
            .query_row("SELECT count(*) FROM triplets", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn chunks_without_triplets_skips_covered_chunks() {
        let conn = boot();
        let doc_id = seed_doc(&conn, "d");
        let ids = seed_chunks(&conn, &doc_id, &["covered", "pending"]);
        stage_triplets(&conn, vec![triplet(&ids[0], "A", "LOCATED_IN", "B")]).unwrap();

        let pending = chunks_without_triplets(&conn, &doc_id).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].chunk_id, ids[1]);
        assert!(
            chunks_without_triplets(&conn, "missing-doc")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn entity_registry_merges_on_the_identity_key() {
        let conn = boot();
        let first = ensure_entity(&conn, "e1", "Ada Lovelace", "PERSON", None).unwrap();
        let again = ensure_entity(&conn, "e2", "Ada Lovelace", "PERSON", Some("math")).unwrap();
        assert_eq!(first, "e1");
        assert_eq!(again, "e1", "the identity key, not the passed id, decides");
        // Same name, different type: a different entity (§8 Jordan rule).
        let other = ensure_entity(&conn, "e3", "Ada Lovelace", "CONCEPT", None).unwrap();
        assert_eq!(other, "e3");
        let (name, subtype): (String, Option<String>) = conn
            .query_row(
                "SELECT canonical_name, subtype FROM entities WHERE entity_id = 'e3'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "Ada Lovelace");
        assert_eq!(subtype, None);
    }

    #[test]
    fn typed_alias_hits_are_exact_and_homographs_coexist() {
        let conn = boot();
        // Aliases carry an FK to their entity — the registry rows come first.
        ensure_entity(&conn, "p1", "Jordan Peele", "PERSON", None).unwrap();
        ensure_entity(&conn, "l1", "Jordan", "LOCATION", None).unwrap();
        ensure_entity(&conn, "p2", "Michael Jordan", "PERSON", None).unwrap();
        assert_eq!(lookup_alias(&conn, "jordan", "PERSON").unwrap(), None);
        assert_eq!(
            upsert_alias(&conn, "jordan", "PERSON", "p1").unwrap(),
            None,
            "fresh alias"
        );
        assert_eq!(
            lookup_alias(&conn, "jordan", "PERSON").unwrap().as_deref(),
            Some("p1")
        );
        // Homograph (§8): same alias, different type — an independent row.
        assert_eq!(
            upsert_alias(&conn, "jordan", "LOCATION", "l1").unwrap(),
            None
        );
        assert_eq!(
            lookup_alias(&conn, "jordan", "LOCATION")
                .unwrap()
                .as_deref(),
            Some("l1")
        );
        // A conflicting same-type mapping reports the existing owner (§7.8).
        assert_eq!(
            upsert_alias(&conn, "jordan", "PERSON", "p2")
                .unwrap()
                .as_deref(),
            Some("p1")
        );
    }

    #[test]
    fn alias_lookup_across_types_returns_every_variant() {
        let conn = boot();
        ensure_entity(&conn, "p1", "Jordan Peele", "PERSON", None).unwrap();
        ensure_entity(&conn, "l1", "Jordan", "LOCATION", None).unwrap();
        ensure_entity(&conn, "r1", "Jordan River", "LOCATION", None).unwrap();
        upsert_alias(&conn, "jordan", "PERSON", "p1").unwrap();
        upsert_alias(&conn, "jordan", "LOCATION", "l1").unwrap();
        upsert_alias(&conn, "jordan river", "LOCATION", "r1").unwrap();

        // §8 Stage 5.2: a homograph yields all its type-variants; ordering is
        // unspecified (the set is what matters), so compare sorted.
        let mut ids = lookup_alias_all_types(&conn, "jordan").unwrap();
        ids.sort();
        assert_eq!(ids, ["l1", "p1"]);
        assert!(lookup_alias_all_types(&conn, "nobody").unwrap().is_empty());
    }

    #[test]
    fn canonical_names_stay_within_the_supertype() {
        let conn = boot();
        ensure_entity(&conn, "p1", "Jordan", "PERSON", None).unwrap();
        ensure_entity(&conn, "l1", "Jordan", "LOCATION", None).unwrap();
        let people = canonical_names(&conn, "PERSON").unwrap();
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].entity_id, "p1");
    }

    #[test]
    fn er_review_dedups_pending_pairs_in_either_order() {
        let conn = boot();
        assert!(er_review_candidate(&conn, "a", "b", 0.9).unwrap());
        assert!(!er_review_candidate(&conn, "b", "a", 0.9).unwrap(), "dup");
        assert!(!er_review_candidate(&conn, "a", "a", 0.9).unwrap(), "self");
        let pending: i64 = conn
            .query_row(
                "SELECT count(*) FROM er_review WHERE status = 'PENDING'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pending, 1);
    }

    #[test]
    fn entity_merge_remaps_aliases_and_is_replay_safe() {
        let conn = boot();
        ensure_entity(&conn, "loser", "Ada", "PERSON", None).unwrap();
        ensure_entity(&conn, "winner", "Ada Lovelace", "PERSON", None).unwrap();
        upsert_alias(&conn, "ada", "PERSON", "loser").unwrap();
        upsert_alias(&conn, "ada lovelace", "PERSON", "winner").unwrap();
        assert!(er_review_candidate(&conn, "loser", "winner", 0.91).unwrap());

        assert!(record_entity_merge(&conn, "loser", "winner", "test").unwrap());
        assert_eq!(resolve_entity(&conn, "loser").unwrap(), "winner");
        assert_eq!(
            lookup_alias(&conn, "ada", "PERSON").unwrap().as_deref(),
            Some("winner")
        );
        assert!(pending_er_reviews(&conn).unwrap().is_empty());
        assert_eq!(entity_merges(&conn).unwrap().len(), 1);
        assert!(!record_entity_merge(&conn, "loser", "winner", "replay").unwrap());
    }

    #[test]
    fn entity_gc_candidates_preserve_first_observation_and_delete_registry_rows() {
        let conn = boot();
        ensure_entity(&conn, "unused", "Unused", "CONCEPT", None).unwrap();
        assert!(record_entity_gc_candidate(&conn, "unused", "2026-09-01 00:00:00").unwrap());
        assert!(!record_entity_gc_candidate(&conn, "unused", "2026-09-02 00:00:00").unwrap());
        assert_eq!(
            due_entity_gc_candidates(&conn, "2026-10-01 00:00:00").unwrap()[0].zero_since,
            "2026-09-01 00:00:00"
        );
        upsert_alias(&conn, "unused", "CONCEPT", "unused").unwrap();
        assert!(execute_entity_gc(&conn, "unused").unwrap());
        assert!(entity_details(&conn, "unused").unwrap().is_none());
        assert!(
            due_entity_gc_candidates(&conn, "9999-12-31 00:00:00")
                .unwrap()
                .is_empty()
        );
        assert!(!execute_entity_gc(&conn, "unused").unwrap());
    }

    #[test]
    fn entity_gc_candidates_are_cancelled_when_mentions_return() {
        let conn = boot();
        ensure_entity(&conn, "used", "Used", "CONCEPT", None).unwrap();
        assert!(record_entity_gc_candidate(&conn, "used", "2026-09-01 00:00:00").unwrap());
        assert!(cancel_entity_gc_candidate(&conn, "used").unwrap());
        assert!(!cancel_entity_gc_candidate(&conn, "used").unwrap());
        assert!(
            due_entity_gc_candidates(&conn, "2026-10-01 00:00:00")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn entity_ids_are_uuidv7_time_ordered() {
        let a = new_entity_id();
        let b = new_entity_id();
        assert_ne!(a, b);
        assert_eq!(uuid::Uuid::parse_str(&a).unwrap().get_version_num(), 7);
        // `Stage::Extract` exists so the stage's job type is real (§6).
        assert_eq!(Stage::Extract.as_str(), "EXTRACT");
    }
}
