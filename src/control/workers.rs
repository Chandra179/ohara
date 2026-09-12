//! Durable worker lifecycle projections owned by the control plane.

use std::time::Duration;

use rusqlite::{Connection, OptionalExtension};

use super::db::{DbError, shift};
use super::models::{WorkerObservation, WorkerState, WorkerStatus};

/// Registers one worker boot. A worker id is unique per process lifetime, so
/// replaying this operation is still safe if startup is retried.
pub(crate) fn register(
    conn: &Connection,
    worker_id: &str,
    process_id: u32,
    now_stamp: &str,
) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO worker_status
            (worker_id, process_id, state, started_at, last_heartbeat_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?4, ?4)
         ON CONFLICT(worker_id) DO UPDATE SET
            process_id = excluded.process_id,
            state = excluded.state,
            started_at = excluded.started_at,
            last_heartbeat_at = excluded.last_heartbeat_at,
            current_stage = NULL,
            current_job_id = NULL,
            last_error = NULL,
            updated_at = excluded.updated_at",
        rusqlite::params![
            worker_id,
            i64::from(process_id),
            WorkerState::Starting.as_str(),
            now_stamp
        ],
    )?;
    Ok(())
}

/// Updates the durable worker lifecycle projection.
pub(crate) fn update_state(
    conn: &Connection,
    worker_id: &str,
    state: WorkerState,
    current_stage: Option<&str>,
    current_job_id: Option<&str>,
    last_error: Option<&str>,
    now_stamp: &str,
) -> Result<(), DbError> {
    let updated = conn.execute(
        "UPDATE worker_status
            SET state = ?2,
                current_stage = ?3,
                current_job_id = ?4,
                last_error = ?5,
                last_heartbeat_at = ?6,
                updated_at = ?6
          WHERE worker_id = ?1",
        rusqlite::params![
            worker_id,
            state.as_str(),
            current_stage,
            current_job_id,
            last_error,
            now_stamp
        ],
    )?;
    if updated != 1 {
        return Err(DbError::WorkerRecord(format!(
            "worker {worker_id:?} is not registered"
        )));
    }
    Ok(())
}

/// Updates only the heartbeat timestamp so long-running stages remain visible.
pub(crate) fn heartbeat(
    conn: &Connection,
    worker_id: &str,
    now_stamp: &str,
) -> Result<(), DbError> {
    let updated = conn.execute(
        "UPDATE worker_status
            SET last_heartbeat_at = ?2, updated_at = ?2
          WHERE worker_id = ?1",
        rusqlite::params![worker_id, now_stamp],
    )?;
    if updated != 1 {
        return Err(DbError::WorkerRecord(format!(
            "worker {worker_id:?} is not registered"
        )));
    }
    Ok(())
}

/// Reads the newest worker projection and applies the stale-heartbeat rule.
pub(crate) fn latest(
    conn: &Connection,
    now_stamp: &str,
    stale_after: Duration,
) -> Result<Option<WorkerObservation>, DbError> {
    let row = conn
        .query_row(
            "SELECT worker_id, process_id, state, started_at, last_heartbeat_at,
                    current_stage, current_job_id, last_error
               FROM worker_status
              ORDER BY last_heartbeat_at DESC, worker_id DESC
              LIMIT 1",
            [],
            |row| {
                let process_id = row.get::<_, i64>(1)?;
                let process_id = u32::try_from(process_id).map_err(|_| {
                    rusqlite::Error::ToSqlConversionFailure(Box::new(DbError::WorkerRecord(
                        format!("invalid process id {process_id}"),
                    )))
                })?;
                let state_value: String = row.get(2)?;
                let state = WorkerState::parse(&state_value)
                    .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
                Ok(WorkerStatus {
                    worker_id: row.get(0)?,
                    process_id,
                    state,
                    started_at: row.get(3)?,
                    last_heartbeat_at: row.get(4)?,
                    current_stage: row.get(5)?,
                    current_job_id: row.get(6)?,
                    last_error: row.get(7)?,
                })
            },
        )
        .optional()?;

    let Some(status) = row else {
        return Ok(None);
    };
    let cutoff = shift(
        now_stamp,
        -i64::try_from(stale_after.as_secs()).unwrap_or(i64::MAX / 2),
    )?;
    Ok(Some(WorkerObservation {
        stale: status.last_heartbeat_at < cutoff,
        status,
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

    use super::*;

    const NOW: &str = "2026-09-12 12:00:00";

    #[test]
    fn worker_status_registers_updates_and_detects_staleness() {
        let db = crate::control::testing::boot_raw();
        register(&db, "worker-a", 42, "2026-09-12 11:59:50").unwrap();
        update_state(
            &db,
            "worker-a",
            WorkerState::Running,
            Some("CLEAN"),
            Some("job-a"),
            None,
            "2026-09-12 11:59:55",
        )
        .unwrap();
        heartbeat(&db, "worker-a", "2026-09-12 11:59:59").unwrap();

        let fresh = latest(&db, NOW, Duration::from_secs(10))
            .unwrap()
            .expect("worker observation");
        assert_eq!(fresh.status.state, WorkerState::Running);
        assert_eq!(fresh.status.current_stage.as_deref(), Some("CLEAN"));
        assert!(!fresh.stale);

        let stale = latest(&db, NOW, Duration::ZERO)
            .unwrap()
            .expect("worker observation");
        assert!(stale.stale);
    }
}
