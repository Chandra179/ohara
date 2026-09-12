//! Entity registry and typed alias operations.

use rusqlite::Connection;

use super::super::db::DbError;
use super::super::models::new_id;

/// The candidate pool used by same-supertype name resolution.
#[derive(Debug, Clone)]
pub struct NameCandidate {
    /// The candidate's surrogate id.
    pub entity_id: String,
    /// Its canonical (registry) name.
    pub canonical_name: String,
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

/// The registry supertype of one entity id, if the entity still exists. Used to
/// keep embedding-based similarity matching within the supertype.
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

/// Mints a uuidv7 entity id (§3: evolving first-class entities get stable
/// surrogates; looked up by `UNIQUE(canonical_name, entity_type)`).
pub(crate) fn new_entity_id() -> String {
    new_id()
}
