//! The boot reconciliation sweep (§7.3, §7.6): makes every crash window that
//! `SQLite` can see recoverable before the worker loop claims anything. The
//! knowledge-plane half of the sweep (§7.3 `has_vector` re-verification) joins
//! with the knowledge store in §15 step 4.

use std::time::Duration;

use rusqlite::Connection;

use super::db::{DbError, shift};
use super::documents;

/// What one sweep did (ops visibility, §13).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// §7.6 deletion intents executed (documents removed; their CASCADEs cleaned
    /// jobs, chunks, triplets, and the intent rows).
    pub deletions_executed: usize,
    /// §5 audit rows pruned past the retention window.
    pub stage_events_pruned: usize,
}

/// Runs the boot sweep (§7.3):
///
/// 1. **Deletion intents** (§7.6): any `deletions` row means a deletion was
///    requested but not finished — execute it. Idempotent: the intent row is
///    removed by the same CASCADE that removes the document, so it re-appears
///    only if the deletion was interrupted before it. The knowledge-plane
///    `delete_doc` precedes the `SQLite` half once the knowledge store exists.
/// 2. **Audit retention** (§5 note): `stage_events` rows older than `retention`
///    are pruned; a zero retention disables pruning.
///
/// Deliberately **not** swept: `RUNNING` jobs with expired leases — the §6 claim
/// reclaims them lazily with the correct single `attempts` increment, and the
/// knowledge-plane re-verification (§7.3) happens then, per job.
///
/// # Errors
/// [`DbError::Sqlite`] on statement failure, or [`DbError::Timestamp`] if
/// `now_stamp` does not follow the §5 format.
pub fn reconcile(
    conn: &Connection,
    now_stamp: &str,
    retention: Duration,
) -> Result<ReconcileReport, DbError> {
    let mut report = ReconcileReport::default();

    for intent in documents::pending_deletions(conn)? {
        if documents::execute_deletion(conn, &intent.doc_id)? {
            report.deletions_executed += 1;
        }
    }

    if !retention.is_zero() {
        let cutoff = shift(
            now_stamp,
            -i64::try_from(retention.as_secs()).unwrap_or(i64::MAX),
        )?;
        report.stage_events_pruned =
            conn.execute("DELETE FROM stage_events WHERE ts < ?1", [&cutoff])?;
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // §10: tests unwrap freely

    use std::time::Duration;

    use super::super::documents;
    use super::super::jobs::record_event;
    use super::super::testing::{boot, seed_doc};
    use super::*;

    const NOW: &str = "2026-09-06 12:00:00";
    const NINETY_DAYS: Duration = Duration::from_hours(90 * 24);

    #[test]
    fn reconcile_executes_pending_deletions_idempotently() {
        let conn = boot();
        let doc_id = seed_doc(&conn, "doomed");
        documents::request_deletion(&conn, &doc_id, Some("user request")).unwrap();

        let report = reconcile(&conn, NOW, NINETY_DAYS).unwrap();

        assert_eq!(report.deletions_executed, 1);
        assert!(documents::get(&conn, &doc_id).unwrap().is_none());

        let again = reconcile(&conn, NOW, NINETY_DAYS).unwrap();
        assert_eq!(
            again.deletions_executed, 0,
            "§7.6: an executed intent cannot re-fire — the row died with the document"
        );
    }

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

        let report = reconcile(&conn, NOW, NINETY_DAYS).unwrap();

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

        let report = reconcile(&conn, NOW, Duration::ZERO).unwrap();

        assert_eq!(report.stage_events_pruned, 0);
        let remaining: i64 = conn
            .query_row("SELECT count(*) FROM stage_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 1);
    }

    #[test]
    fn reconcile_on_a_fresh_store_is_a_no_op() {
        let conn = boot();
        let report = reconcile(&conn, NOW, NINETY_DAYS).unwrap();
        assert_eq!(
            report,
            ReconcileReport::default(),
            "a clean boot reconciles nothing"
        );
    }
}
