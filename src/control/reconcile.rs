//! The boot reconciliation sweep (§7.3, §7.6): makes every crash window that
//! `SQLite` can see recoverable before the worker loop claims anything. The
//! knowledge-plane half of the sweep (§7.3 `has_vector` re-verification) joins
//! with the knowledge store in §15 step 4.

use std::time::Duration;

use rusqlite::Connection;

use super::db::{DbError, shift};

/// What one sweep did (ops visibility, §13).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// §7.6 deletion intents executed (documents removed; their CASCADEs cleaned
    /// jobs, chunks, triplets, and the intent rows).
    pub deletions_executed: usize,
    /// §5 audit rows pruned past the retention window.
    pub stage_events_pruned: usize,
}

/// Prunes retained audit events without touching deletion intents.
///
/// The worker uses this after it has completed the knowledge-first deletion
/// protocol. Keeping the operation separate prevents a boot sweep from
/// deleting a `SQLite` document before its knowledge indexes are cleaned.
pub(crate) fn reconcile_retention(
    conn: &Connection,
    now_stamp: &str,
    retention: Duration,
) -> Result<ReconcileReport, DbError> {
    Ok(ReconcileReport {
        stage_events_pruned: prune_stage_events(conn, now_stamp, retention)?,
        ..ReconcileReport::default()
    })
}

fn prune_stage_events(
    conn: &Connection,
    now_stamp: &str,
    retention: Duration,
) -> Result<usize, DbError> {
    if retention.is_zero() {
        return Ok(0);
    }
    let cutoff = shift(
        now_stamp,
        -i64::try_from(retention.as_secs()).unwrap_or(i64::MAX),
    )?;
    Ok(conn.execute("DELETE FROM stage_events WHERE ts < ?1", [&cutoff])?)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // §10: tests unwrap freely

    use std::time::Duration;

    use super::super::jobs::record_event;
    use super::super::testing::boot_raw as boot;
    use super::*;

    const NOW: &str = "2026-09-06 12:00:00";
    const NINETY_DAYS: Duration = Duration::from_hours(90 * 24);

    #[test]
    fn reconcile_prunes_stage_events_past_retention() {
        let conn = boot();
        record_event(&conn, Some("d"), Some("j"), Some("SCRAPE"), "DONE", None).unwrap(); // ts = CURRENT_TIMESTAMP (now)
        conn.execute(
            "INSERT INTO stage_events (doc_id, outcome, ts)
             VALUES ('ancient', 'DEAD', '2020-01-01 00:00:00')",
            [],
        )
        .unwrap();

        let report = reconcile_retention(&conn, NOW, NINETY_DAYS).unwrap();

        assert_eq!(report.stage_events_pruned, 1, "§5 note: 90-day retention");
        let remaining: i64 = conn
            .query_row("SELECT count(*) FROM stage_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 1, "recent audit rows survive");
    }

    #[test]
    fn zero_retention_disables_pruning() {
        let conn = boot();
        conn.execute(
            "INSERT INTO stage_events (doc_id, outcome, ts)
             VALUES ('ancient', 'DEAD', '2020-01-01 00:00:00')",
            [],
        )
        .unwrap();

        let report = reconcile_retention(&conn, NOW, Duration::ZERO).unwrap();

        assert_eq!(report.stage_events_pruned, 0);
        let remaining: i64 = conn
            .query_row("SELECT count(*) FROM stage_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 1);
    }

    #[test]
    fn reconcile_on_a_fresh_store_is_a_no_op() {
        let conn = boot();
        let report = reconcile_retention(&conn, NOW, NINETY_DAYS).unwrap();
        assert_eq!(
            report,
            ReconcileReport::default(),
            "a clean boot reconciles nothing"
        );
    }
}
