//! `SQLite` connections: §5 pragmas, the one `now()` helper (the §5 timestamp rule),
//! and the versioned migration runner. This file is the only place that knows the
//! SQL dialect (`control` owns the store, §1.2.2).

use std::path::Path;

use rusqlite::Connection;

/// Native UTC timestamp format (§5): lexicographic order must equal chronological
/// order — lease expiry and backoff comparisons depend on it.
const TS_FMT: &str = "%Y-%m-%d %H:%M:%S";

/// Migration files, applied in order; the filename stem is the schema version.
const MIGRATIONS: &[(&str, &str)] = &[
    (
        "0001_initial",
        include_str!("../../migrations/0001_initial.sql"),
    ),
    (
        "0002_llm_usage",
        include_str!("../../migrations/0002_llm_usage.sql"),
    ),
    (
        "0003_entity_gc",
        include_str!("../../migrations/0003_entity_gc.sql"),
    ),
];

/// Control-plane errors.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// A `SQLite` statement, pragma, or transaction failed.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A migration failed mid-apply; its transaction is rolled back and the version
    /// is not recorded.
    #[error("migration {version} failed")]
    Migration {
        /// The migration filename stem that failed.
        version: String,
        /// The underlying `SQLite` failure.
        #[source]
        source: rusqlite::Error,
    },
    /// A stored or computed timestamp does not follow the §5 format.
    #[error("malformed timestamp {value:?}: expected {fmt:?}")]
    Timestamp {
        /// The offending value.
        value: String,
        /// The expected format.
        fmt: &'static str,
    },
    /// An entity referenced by an operator merge does not exist.
    #[error("entity not found: {entity_id}")]
    EntityNotFound {
        /// Missing entity id.
        entity_id: String,
    },
    /// An entity cannot be merged into itself.
    #[error("cannot merge entity {entity_id} into itself")]
    MergeSelf {
        /// Repeated entity id.
        entity_id: String,
    },
    /// An entity merge would cross the closed ontology's supertypes.
    #[error("entity types do not match: {loser_id}={loser_type}, {winner_id}={winner_type}")]
    EntityTypeMismatch {
        /// Loser entity id.
        loser_id: String,
        /// Loser entity type.
        loser_type: String,
        /// Winner entity id.
        winner_id: String,
        /// Winner entity type.
        winner_type: String,
    },
    /// The merge audit already records a different winner for the loser.
    #[error("entity {loser_id} already merges into {existing_winner}, not {requested_winner}")]
    MergeConflict {
        /// Loser entity id.
        loser_id: String,
        /// Existing winner.
        existing_winner: String,
        /// Requested winner.
        requested_winner: String,
    },
    /// The merge audit contains a cycle and cannot be resolved safely.
    #[error("entity merge cycle at {entity_id}")]
    MergeCycle {
        /// Entity id where resolution revisited a node.
        entity_id: String,
    },
}

/// Opaque control-plane database handle.
///
/// `SQLite` stays an implementation detail of the control plane: callers use the
/// domain operations re-exported from [`crate::control`] instead of receiving a
/// vendor connection they can query directly.
pub struct ControlDb {
    connection: Connection,
}

impl std::fmt::Debug for ControlDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControlDb").finish_non_exhaustive()
    }
}

impl ControlDb {
    /// Borrows the `SQLite` connection for control-plane implementation code.
    pub(crate) fn raw(&self) -> &Connection {
        &self.connection
    }

    /// Converts the handle into its connection for in-crate unit-test fixtures.
    #[cfg(test)]
    pub(crate) fn into_raw(self) -> Connection {
        self.connection
    }
}

/// Opens (creating if needed) the control store at `path`, sets the §5 connection
/// pragmas, and applies any pending migrations.
///
/// # Errors
/// [`DbError::Sqlite`] on open or pragma failure; [`DbError::Migration`] naming the
/// version if a migration fails mid-apply.
pub fn connect(path: &Path) -> Result<ControlDb, DbError> {
    let conn = Connection::open(path)?;
    set_pragmas(&conn)?;
    migrate(&conn)?;
    Ok(ControlDb { connection: conn })
}

/// Captures a consistent `SQLite` snapshot into a new file.
///
/// The caller must already have quiesced the worker and acquired the runtime
/// lock. The checkpoint removes the WAL before `VACUUM INTO` writes the
/// snapshot, so the destination never depends on live sidecar files.
///
/// # Errors
/// [`DbError::Sqlite`] if checkpointing or snapshot creation fails.
pub fn backup_to(db: &ControlDb, destination: &Path) -> Result<(), DbError> {
    db.connection
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))?;
    db.connection
        .execute("VACUUM INTO ?1", [destination.to_string_lossy().as_ref()])?;
    Ok(())
}

/// The §5 pragmas, on every connection: WAL, `NORMAL` synchronous, `foreign_keys = ON`
/// (`SQLite` defaults it off — without it the schema's CASCADEs are inert), 5 s busy timeout.
fn set_pragmas(conn: &Connection) -> Result<(), DbError> {
    // journal_mode returns the resulting mode (a row), so it goes through query_row.
    conn.query_row("PRAGMA journal_mode = WAL", [], |row| {
        row.get::<_, String>(0)
    })?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5_000)?;
    Ok(())
}

/// Applies pending migrations in order, one transaction per migration.
fn migrate(conn: &Connection) -> Result<(), DbError> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version    TEXT PRIMARY KEY,
             applied_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
         )",
        [],
    )?;
    for (version, sql) in MIGRATIONS {
        let applied: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = ?1)",
            [version],
            |row| row.get(0),
        )?;
        if applied {
            continue;
        }
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql).map_err(|source| DbError::Migration {
            version: (*version).to_string(),
            source,
        })?;
        tx.execute(
            "INSERT INTO schema_migrations (version) VALUES (?1)",
            [version],
        )?;
        tx.commit()?;
    }
    Ok(())
}

/// The one timestamp helper (§5): UTC `YYYY-MM-DD HH:MM:SS`, `CURRENT_TIMESTAMP`'s
/// format. All application writes to `TIMESTAMP` columns go through this or
/// [`now_plus`].
pub(crate) fn now() -> String {
    chrono::Utc::now().format(TS_FMT).to_string()
}

/// `stamp` advanced by `secs` — lease expiries and backoff dues, in the same §5 format.
///
/// # Errors
/// [`DbError::Timestamp`] if `stamp` is not in the §5 format.
pub(crate) fn now_plus(stamp: &str, secs: u64) -> Result<String, DbError> {
    shift(stamp, i64::try_from(secs).unwrap_or(i64::MAX / 2))
}

/// `stamp` shifted by signed `secs` (negative = into the past — retention cutoffs).
/// Saturates far beyond any sensible lease/backoff/retention horizon rather than
/// overflowing: chrono panics on out-of-range deltas.
///
/// # Errors
/// [`DbError::Timestamp`] if `stamp` is not in the §5 format.
pub(crate) fn shift(stamp: &str, secs: i64) -> Result<String, DbError> {
    let t =
        chrono::NaiveDateTime::parse_from_str(stamp, TS_FMT).map_err(|_| DbError::Timestamp {
            value: stamp.to_string(),
            fmt: TS_FMT,
        })?;
    let t = t
        .checked_add_signed(chrono::Duration::seconds(secs))
        .ok_or(DbError::Timestamp {
            value: stamp.to_string(),
            fmt: TS_FMT,
        })?;
    Ok(t.format(TS_FMT).to_string())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // §10: tests unwrap freely

    use super::*;

    #[test]
    fn now_is_native_sqlite_utc_format() {
        let stamp = now();
        assert_eq!(stamp.len(), 19, "unexpected format: {stamp}");
        let bytes = stamp.as_bytes();
        assert_eq!(bytes[4], b'-');
        assert_eq!(bytes[7], b'-');
        assert_eq!(bytes[10], b' ');
        assert_eq!(bytes[13], b':');
        assert_eq!(bytes[16], b':');
        // Lexicographic == chronological (§5) against a fixed earlier instant.
        assert!(stamp.as_str() > "2026-01-01 00:00:00");
    }

    #[test]
    fn now_plus_advances_and_preserves_format() {
        let stamp = "2026-09-06 23:59:30";
        let advanced = now_plus(stamp, 60).expect("valid stamp");
        assert_eq!(advanced, "2026-09-07 00:00:30");
    }

    #[test]
    fn now_plus_rejects_malformed_stamps() {
        let err = now_plus("not-a-timestamp", 60).expect_err("must reject");
        assert!(matches!(err, DbError::Timestamp { .. }));
    }

    #[test]
    fn connect_boots_a_fresh_store() {
        let conn = connect(Path::new(":memory:")).expect("boot");
        let versions: i64 = conn
            .raw()
            .query_row("SELECT count(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .expect("schema_migrations");
        assert_eq!(
            versions, 3,
            "exactly one row per applied migration recorded"
        );
        let jobs: i64 = conn
            .raw()
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'jobs'",
                [],
                |row| row.get(0),
            )
            .expect("sqlite_master");
        assert_eq!(jobs, 1);
    }

    #[test]
    fn backup_to_writes_a_readable_snapshot() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let source = dir.path().join("source.db");
        let destination = dir.path().join("backup.db");
        let db = connect(&source).expect("source store");
        db.connection
            .execute("CREATE TABLE backup_probe (value TEXT)", [])
            .expect("probe table");
        db.connection
            .execute("INSERT INTO backup_probe VALUES ('ok')", [])
            .expect("probe row");

        backup_to(&db, &destination).expect("snapshot");
        let snapshot = Connection::open(destination).expect("open snapshot");
        let value: String = snapshot
            .query_row("SELECT value FROM backup_probe", [], |row| row.get(0))
            .expect("probe row in snapshot");
        assert_eq!(value, "ok");
    }
}
