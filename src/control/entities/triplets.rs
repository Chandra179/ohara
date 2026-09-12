//! Control-plane triplet staging and replay reads.

use rusqlite::Connection;

use super::super::db::DbError;
use super::super::documents::ChunkText;

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

/// A stored `triplets` row (§5), read back for the §7.3 resume re-merge: a
/// crash between staging and the graph write heals by re-merging rows
/// idempotently — never by re-paying the LLM (§7.7).
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
