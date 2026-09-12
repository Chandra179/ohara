//! Read models assembled inside the control plane for operator surfaces.
//!
//! These queries intentionally return presentation-neutral domain records. The
//! transport layer maps them to JSON, while `SQLite` ownership remains here.

use std::collections::BTreeMap;

use rusqlite::Connection;

use super::DbError;
use super::models::DocStatus;

/// Maximum page size accepted by the local document read model.
pub const MAX_DOCUMENT_PAGE_SIZE: usize = 100;

/// A bounded document-list query.
#[derive(Debug, Clone, Default)]
pub struct DocumentListQuery {
    /// Maximum number of rows to return.
    pub limit: usize,
    /// The last document id returned by the previous page.
    pub cursor: Option<String>,
    /// Optional exact durable document status filter.
    pub status: Option<DocStatus>,
    /// Optional case-insensitive title or URL substring.
    pub search: Option<String>,
}

/// A document summary safe to expose to an operator surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentListItem {
    /// Stable document id.
    pub doc_id: String,
    /// The registered source URL.
    pub source_url: String,
    /// Extracted title, when Stage 2 has produced one.
    pub title: Option<String>,
    /// Durable document milestone/status.
    pub status: String,
    /// Number of persisted chunks.
    pub chunk_count: u64,
    /// Document registration timestamp.
    pub created_at: String,
    /// Last successful or terminal processing timestamp.
    pub last_processed_at: Option<String>,
    /// Last recorded failure detail.
    pub error: Option<String>,
}

/// One page of document summaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentPage {
    /// Rows in descending stable-id order.
    pub items: Vec<DocumentListItem>,
    /// Cursor for the next page, if more rows remain.
    pub next_cursor: Option<String>,
}

/// A queue row joined with the document metadata needed by the overview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueItem {
    /// Stable job id.
    pub job_id: String,
    /// Stable document id.
    pub doc_id: String,
    /// Extracted title, when available.
    pub title: Option<String>,
    /// Registered source URL.
    pub source_url: String,
    /// Current durable document status.
    pub document_status: String,
    /// Pipeline stage represented by this job.
    pub stage: String,
    /// Job execution status.
    pub job_status: String,
    /// Last queue transition timestamp.
    pub updated_at: String,
    /// Last job failure detail, when present.
    pub error: Option<String>,
}

/// Read model for the overview page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverviewSnapshot {
    /// Document totals grouped by their durable status.
    pub documents_by_status: BTreeMap<String, u64>,
    /// A bounded view of non-terminal queue work and dead jobs.
    pub queue: Vec<QueueItem>,
}

/// One entity summary used by review list and preview read models.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntitySummary {
    /// Stable entity id.
    pub entity_id: String,
    /// Canonical display name.
    pub canonical_name: String,
    /// Closed ontology type.
    pub entity_type: String,
    /// Number of registered aliases in the control plane.
    pub alias_count: u64,
}

/// A pending entity-review row with both candidate entities hydrated.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityReviewItem {
    /// Review row id.
    pub review_id: i64,
    /// First candidate captured by the detector.
    pub entity_a: EntitySummary,
    /// Second candidate captured by the detector.
    pub entity_b: EntitySummary,
    /// Similarity score that caused the review.
    pub score: Option<f64>,
}

/// Reads one bounded page of document summaries.
pub fn list_documents(
    conn: &Connection,
    query: &DocumentListQuery,
) -> Result<DocumentPage, DbError> {
    let status = query.status.map(DocStatus::as_str);
    let search = query.search.as_deref().map(|value| format!("%{value}%"));
    let sql_limit = i64::try_from(query.limit.saturating_add(1)).unwrap_or(i64::MAX);
    let mut statement = conn.prepare(
        "SELECT doc_id, source_url, title, status, chunk_count, created_at,
                last_processed_at, error
           FROM documents
          WHERE (?1 IS NULL OR doc_id < ?1)
            AND (?2 IS NULL OR status = ?2)
            AND (?3 IS NULL OR title LIKE ?3 OR source_url LIKE ?3)
          ORDER BY doc_id DESC
          LIMIT ?4",
    )?;
    let rows = statement.query_map(
        rusqlite::params![query.cursor, status, search, sql_limit],
        |row| {
            Ok(DocumentListItem {
                doc_id: row.get(0)?,
                source_url: row.get(1)?,
                title: row.get(2)?,
                status: row.get(3)?,
                chunk_count: row.get::<_, i64>(4)?.cast_unsigned(),
                created_at: row.get(5)?,
                last_processed_at: row.get(6)?,
                error: row.get(7)?,
            })
        },
    )?;
    let mut items = rows.collect::<Result<Vec<_>, _>>()?;
    let next_cursor = if items.len() > query.limit {
        items.truncate(query.limit);
        items.last().map(|item| item.doc_id.clone())
    } else {
        None
    };
    Ok(DocumentPage { items, next_cursor })
}

/// Reads document status counts and a bounded queue projection.
pub fn overview(conn: &Connection, queue_limit: usize) -> Result<OverviewSnapshot, DbError> {
    let mut status_statement =
        conn.prepare("SELECT status, count(*) FROM documents GROUP BY status ORDER BY status")?;
    let status_rows = status_statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?.cast_unsigned(),
        ))
    })?;
    let documents_by_status = status_rows.collect::<Result<BTreeMap<_, _>, _>>()?;

    let sql_limit = i64::try_from(queue_limit).unwrap_or(i64::MAX);
    let mut queue_statement = conn.prepare(
        "SELECT j.job_id, j.doc_id, d.title, d.source_url, d.status, j.stage,
                j.status, j.updated_at, j.last_error
           FROM jobs AS j
           JOIN documents AS d ON d.doc_id = j.doc_id
          WHERE j.status <> 'DONE' AND d.status <> 'ARCHIVED'
          ORDER BY CASE j.status WHEN 'RUNNING' THEN 0
                                 WHEN 'PENDING' THEN 1
                                 ELSE 2 END,
                   j.priority, j.updated_at DESC, j.job_id
          LIMIT ?1",
    )?;
    let queue_rows = queue_statement.query_map([sql_limit], |row| {
        Ok(QueueItem {
            job_id: row.get(0)?,
            doc_id: row.get(1)?,
            title: row.get(2)?,
            source_url: row.get(3)?,
            document_status: row.get(4)?,
            stage: row.get(5)?,
            job_status: row.get(6)?,
            updated_at: row.get(7)?,
            error: row.get(8)?,
        })
    })?;
    let queue = queue_rows.collect::<Result<Vec<_>, _>>()?;
    Ok(OverviewSnapshot {
        documents_by_status,
        queue,
    })
}

/// Reads pending entity reviews and hydrates both registry rows.
pub fn entity_review_items(conn: &Connection) -> Result<Vec<EntityReviewItem>, DbError> {
    let mut statement = conn.prepare(
        "SELECT r.id,
                a.entity_id, a.canonical_name, a.entity_type,
                (SELECT count(*) FROM entity_aliases aa WHERE aa.entity_id = a.entity_id),
                b.entity_id, b.canonical_name, b.entity_type,
                (SELECT count(*) FROM entity_aliases ab WHERE ab.entity_id = b.entity_id),
                r.score
           FROM er_review AS r
           JOIN entities AS a ON a.entity_id = r.entity_a
           JOIN entities AS b ON b.entity_id = r.entity_b
          WHERE r.status = 'PENDING'
          ORDER BY r.created_at, r.id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(EntityReviewItem {
            review_id: row.get(0)?,
            entity_a: EntitySummary {
                entity_id: row.get(1)?,
                canonical_name: row.get(2)?,
                entity_type: row.get(3)?,
                alias_count: row.get::<_, i64>(4)?.cast_unsigned(),
            },
            entity_b: EntitySummary {
                entity_id: row.get(5)?,
                canonical_name: row.get(6)?,
                entity_type: row.get(7)?,
                alias_count: row.get::<_, i64>(8)?.cast_unsigned(),
            },
            score: row.get(9)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::{DocumentListQuery, entity_review_items, list_documents, overview};
    use crate::control::{DocStatus, connect};

    #[test]
    fn document_pages_use_a_stable_cursor_and_preserve_statuses() {
        let db = connect(std::path::Path::new(":memory:")).expect("database");
        for (id, status) in [("doc-b", "FAILED_QUALITY"), ("doc-a", "INDEXED")] {
            db.raw()
                .execute(
                    "INSERT INTO documents
                        (doc_id, source_url, source_url_normalized, raw_file_path, status, pipeline_version)
                     VALUES (?1, ?2, ?2, ?3, ?4, 'test')",
                    rusqlite::params![
                        id,
                        format!("https://{id}.test"),
                        format!("raw/{id}"),
                        status
                    ],
                )
                .expect("document");
        }

        let first = list_documents(
            db.raw(),
            &DocumentListQuery {
                limit: 1,
                ..DocumentListQuery::default()
            },
        )
        .expect("first page");
        assert_eq!(first.items[0].status, DocStatus::FailedQuality.as_str());
        assert_eq!(first.next_cursor.as_deref(), Some("doc-b"));

        let second = list_documents(
            db.raw(),
            &DocumentListQuery {
                cursor: first.next_cursor,
                limit: 1,
                ..DocumentListQuery::default()
            },
        )
        .expect("second page");
        assert_eq!(second.items[0].status, DocStatus::Indexed.as_str());
        assert!(second.next_cursor.is_none());
    }

    #[test]
    fn overview_returns_counts_and_only_active_queue_rows() {
        let db = connect(std::path::Path::new(":memory:")).expect("database");
        db.raw()
            .execute(
                "INSERT INTO documents
                    (doc_id, source_url, source_url_normalized, raw_file_path, status, pipeline_version)
                 VALUES ('doc-1', 'https://one.test', 'https://one.test', 'raw/one', 'NEW', 'test')",
                [],
            )
            .expect("document");
        db.raw()
            .execute(
                "INSERT INTO jobs (job_id, doc_id, stage, status) VALUES
                    ('job-pending', 'doc-1', 'SCRAPE', 'PENDING'),
                    ('job-done', 'doc-1', 'CLEAN', 'DONE')",
                [],
            )
            .expect("jobs");

        let snapshot = overview(db.raw(), 10).expect("overview");
        assert_eq!(snapshot.documents_by_status["NEW"], 1);
        assert_eq!(snapshot.queue.len(), 1);
        assert_eq!(snapshot.queue[0].job_status, "PENDING");
    }

    #[test]
    fn entity_reviews_hydrate_both_candidates() {
        let db = connect(std::path::Path::new(":memory:")).expect("database");
        for (id, name) in [("entity-a", "Ohara"), ("entity-b", "O'hara")] {
            db.raw()
                .execute(
                    "INSERT INTO entities (entity_id, canonical_name, entity_type)
                     VALUES (?1, ?2, 'PRODUCT')",
                    rusqlite::params![id, name],
                )
                .expect("entity");
        }
        db.raw()
            .execute(
                "INSERT INTO er_review (entity_a, entity_b, score, status)
                 VALUES ('entity-a', 'entity-b', 0.91, 'PENDING')",
                [],
            )
            .expect("review");

        let reviews = entity_review_items(db.raw()).expect("reviews");
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].entity_a.canonical_name, "Ohara");
        assert_eq!(reviews[0].score, Some(0.91));
    }
}
