//! Durable control-plane metrics derived from `SQLite` state and audit rows (§13).
//!
//! This module deliberately returns domain data rather than exposing SQL rows.
//! Filesystem usage and operator presentation remain outside the control plane.

use std::collections::BTreeMap;

use rusqlite::Connection;
use serde::Serialize;

use super::db::DbError;

/// Outcome recorded for one LLM completion attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmUsageOutcome {
    /// The provider returned a completion.
    Succeeded,
    /// The provider call failed or returned an invalid response.
    Failed,
}

impl LlmUsageOutcome {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "SUCCEEDED",
            Self::Failed => "FAILED",
        }
    }
}

/// One immutable LLM completion-attempt record.
#[derive(Debug, Clone, Copy)]
pub struct LlmUsageEvent<'a> {
    /// Operation that initiated the call, such as extract or synthesis.
    pub operation: &'a str,
    /// Document associated with extraction, if any.
    pub doc_id: Option<&'a str>,
    /// Job associated with extraction, if any.
    pub job_id: Option<&'a str>,
    /// Provider name declared by the LLM Adapter.
    pub provider: &'a str,
    /// Model requested for this attempt.
    pub model: &'a str,
    /// Provider outcome.
    pub outcome: LlmUsageOutcome,
    /// Prompt tokens reported by the provider.
    pub prompt_tokens: u64,
    /// Completion tokens reported by the provider.
    pub completion_tokens: u64,
    /// Estimated cost in millionths of a US dollar.
    pub estimated_cost_micros: u64,
    /// Provider error, when the attempt failed.
    pub error: Option<&'a str>,
}

/// A consistent read of the control-plane metrics used by the operator view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MetricsSnapshot {
    /// UTC timestamp at which the snapshot's due-date checks were evaluated.
    pub captured_at: String,
    /// Number of documents grouped by their milestone/status.
    pub documents_by_status: BTreeMap<String, u64>,
    /// Number of jobs grouped by pipeline stage and execution status.
    pub jobs_by_stage_status: BTreeMap<String, BTreeMap<String, u64>>,
    /// Number of retained audit events grouped by outcome.
    pub events_by_outcome: BTreeMap<String, u64>,
    /// Number of retained audit events grouped by stage.
    pub events_by_stage: BTreeMap<String, u64>,
    /// Number of unresolved cross-document entity-review candidates.
    pub pending_er_reviews: u64,
    /// Number of documents currently eligible for a conditional re-crawl.
    pub due_for_recrawl: u64,
    /// Durable LLM completion-attempt usage.
    pub llm_usage: LlmUsageSnapshot,
}

/// Aggregated durable LLM usage for the operator view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmUsageSnapshot {
    /// Number of recorded attempts.
    pub calls: u64,
    /// Number of successful attempts.
    pub successful_calls: u64,
    /// Number of failed attempts.
    pub failed_calls: u64,
    /// Total prompt tokens reported by providers.
    pub prompt_tokens: u64,
    /// Total completion tokens reported by providers.
    pub completion_tokens: u64,
    /// Estimated cost in millionths of a US dollar.
    pub estimated_cost_micros: u64,
}

/// Reads a metrics snapshot without leaking the `SQLite` connection to callers.
///
/// # Errors
/// [`DbError::Sqlite`] if an aggregate query or row decode fails.
pub fn snapshot(conn: &Connection, now_stamp: &str) -> Result<MetricsSnapshot, DbError> {
    let documents_by_status = grouped_counts(
        conn,
        "SELECT status, count(*) FROM documents GROUP BY status ORDER BY status",
    )?;
    let jobs_by_stage_status = grouped_job_counts(conn)?;
    let events_by_outcome = grouped_counts(
        conn,
        "SELECT COALESCE(outcome, '(none)'), count(*) FROM stage_events
          GROUP BY outcome ORDER BY outcome",
    )?;
    let events_by_stage = grouped_counts(
        conn,
        "SELECT COALESCE(stage, '(none)'), count(*) FROM stage_events
          GROUP BY stage ORDER BY stage",
    )?;
    let pending_er_reviews = scalar_count(
        conn,
        "SELECT count(*) FROM er_review WHERE status = 'PENDING'",
    )?;
    let due_for_recrawl = scalar_count_with_arg(
        conn,
        "SELECT count(*) FROM documents
          WHERE next_crawl_at IS NOT NULL AND next_crawl_at <= ?1
            AND status NOT IN ('FAILED_QUALITY', 'FAILED', 'ARCHIVED')",
        now_stamp,
    )?;
    let llm_usage = usage_snapshot(conn)?;

    Ok(MetricsSnapshot {
        captured_at: now_stamp.to_string(),
        documents_by_status,
        jobs_by_stage_status,
        events_by_outcome,
        events_by_stage,
        pending_er_reviews,
        due_for_recrawl,
        llm_usage,
    })
}

/// Appends one completion attempt to the durable usage ledger.
pub fn record_llm_usage(conn: &Connection, event: &LlmUsageEvent<'_>) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO llm_usage
            (operation, doc_id, job_id, provider, model, outcome,
             prompt_tokens, completion_tokens, estimated_cost_micros, error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            event.operation,
            event.doc_id,
            event.job_id,
            event.provider,
            event.model,
            event.outcome.as_str(),
            i64::try_from(event.prompt_tokens).unwrap_or(i64::MAX),
            i64::try_from(event.completion_tokens).unwrap_or(i64::MAX),
            i64::try_from(event.estimated_cost_micros).unwrap_or(i64::MAX),
            event.error,
        ],
    )?;
    Ok(())
}

fn usage_snapshot(conn: &Connection) -> Result<LlmUsageSnapshot, DbError> {
    conn.query_row(
        "SELECT
            count(*),
            COALESCE(sum(CASE WHEN outcome = 'SUCCEEDED' THEN 1 ELSE 0 END), 0),
            COALESCE(sum(CASE WHEN outcome = 'FAILED' THEN 1 ELSE 0 END), 0),
            COALESCE(sum(prompt_tokens), 0),
            COALESCE(sum(completion_tokens), 0),
            COALESCE(sum(estimated_cost_micros), 0)
         FROM llm_usage",
        [],
        |row| {
            Ok(LlmUsageSnapshot {
                calls: row.get::<_, i64>(0)?.cast_unsigned(),
                successful_calls: row.get::<_, i64>(1)?.cast_unsigned(),
                failed_calls: row.get::<_, i64>(2)?.cast_unsigned(),
                prompt_tokens: row.get::<_, i64>(3)?.cast_unsigned(),
                completion_tokens: row.get::<_, i64>(4)?.cast_unsigned(),
                estimated_cost_micros: row.get::<_, i64>(5)?.cast_unsigned(),
            })
        },
    )
    .map_err(DbError::from)
}

fn grouped_counts(conn: &Connection, sql: &str) -> Result<BTreeMap<String, u64>, DbError> {
    let mut statement = conn.prepare(sql)?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?.cast_unsigned(),
        ))
    })?;
    rows.collect::<Result<BTreeMap<_, _>, _>>()
        .map_err(DbError::from)
}

fn grouped_job_counts(
    conn: &Connection,
) -> Result<BTreeMap<String, BTreeMap<String, u64>>, DbError> {
    let mut statement = conn.prepare(
        "SELECT stage, status, count(*) FROM jobs
          GROUP BY stage, status ORDER BY stage, status",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?.cast_unsigned(),
        ))
    })?;
    let mut grouped = BTreeMap::new();
    for row in rows {
        let (stage, status, count) = row?;
        grouped
            .entry(stage)
            .or_insert_with(BTreeMap::new)
            .insert(status, count);
    }
    Ok(grouped)
}

fn scalar_count(conn: &Connection, sql: &str) -> Result<u64, DbError> {
    Ok(conn
        .query_row(sql, [], |row| row.get::<_, i64>(0))?
        .cast_unsigned())
}

fn scalar_count_with_arg(conn: &Connection, sql: &str, arg: &str) -> Result<u64, DbError> {
    Ok(conn
        .query_row(sql, [arg], |row| row.get::<_, i64>(0))?
        .cast_unsigned())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::snapshot;
    use crate::control::db::connect;

    #[test]
    fn snapshot_groups_control_state_and_due_documents() {
        let db = connect(std::path::Path::new(":memory:")).expect("memory db");
        let conn = db.raw();
        conn.execute(
            "INSERT INTO documents
                (doc_id, source_url, source_url_normalized, raw_file_path, status, next_crawl_at, pipeline_version)
             VALUES ('doc-1', 'https://a.test', 'https://a.test', 'raw/a', 'INDEXED', '2026-09-10 10:00:00', 'test')",
            [],
        )
        .expect("document");
        conn.execute(
            "INSERT INTO jobs (job_id, doc_id, stage, status) VALUES ('job-1', 'doc-1', 'EXTRACT', 'DONE')",
            [],
        )
        .expect("job");
        conn.execute(
            "INSERT INTO stage_events (doc_id, job_id, stage, outcome) VALUES ('doc-1', 'job-1', 'EXTRACT', 'DONE')",
            [],
        )
        .expect("event");
        conn.execute(
            "INSERT INTO er_review (entity_a, entity_b, score, status) VALUES ('a', 'b', 0.9, 'PENDING')",
            [],
        )
        .expect("review");

        let metrics = snapshot(conn, "2026-09-10 11:00:00").expect("metrics");
        assert_eq!(metrics.captured_at, "2026-09-10 11:00:00");
        assert_eq!(metrics.documents_by_status.get("INDEXED"), Some(&1));
        assert_eq!(metrics.jobs_by_stage_status["EXTRACT"]["DONE"], 1);
        assert_eq!(metrics.events_by_outcome["DONE"], 1);
        assert_eq!(metrics.events_by_stage["EXTRACT"], 1);
        assert_eq!(metrics.pending_er_reviews, 1);
        assert_eq!(metrics.due_for_recrawl, 1);
    }
}
