//! Integration (§14): the worker loop end-to-end with injected ports — a canned
//! [`Fetcher`] fake and the real readability extractor drive a document from
//! `NEW` through `SCRAPED` to `CLEANED` through the real §6 machinery
//! ([`ohara::pipeline::Worker::tick_once`]).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // §10: tests unwrap freely

use std::path::Path;
use std::sync::Arc;

use ohara::config::Config;
use ohara::control::{self, DocStatus};
use ohara::engine::{
    FetchCapabilities, FetchError, FetchPolicy, FetchedDoc, Fetcher, NormalizedUrl,
};
use ohara::pipeline::{ExtractError, ExtractedArticle, Extractor, Worker};

const NOW: &str = "2026-09-06 12:00:00";

/// A small but complete article: passes every §8 Stage 2 gate.
const ARTICLE_HTML: &str = r"<html><head><title>On SQLite</title></head><body>
    <article><p>SQLite is an embedded database engine that stores data in a single
    cross-platform file and needs no separate server process at all. Developers love
    it because the deployment story could hardly be simpler for applications of
    almost every conceivable shape and size. The library reads and writes directly
    to ordinary disk files, and the complete database with multiple tables, indices,
    triggers, and views lives inside one portable file.</p></article></body></html>";

/// The fetcher port fake (§14).
enum Fake {
    Html(&'static str),
    NotFound,
}

struct FakeFetcher {
    result: Fake,
}

#[async_trait::async_trait]
impl Fetcher for FakeFetcher {
    fn capabilities(&self) -> FetchCapabilities {
        FetchCapabilities {
            js_rendering: false,
            stealth: false,
        }
    }

    async fn fetch_with_policy(
        &self,
        _url: &NormalizedUrl,
        _policy: &FetchPolicy,
    ) -> Result<FetchedDoc, FetchError> {
        match &self.result {
            Fake::Html(html) => Ok(FetchedDoc {
                html: (*html).to_string(),
                js_executed: false,
                final_url: "https://example.com/a".to_string(),
                status: 200,
                content_type: Some("text/html; charset=utf-8".to_string()),
                etag: None,
                last_modified: None,
                fetched_at: "2026-09-06T12:00:00+00:00".to_string(),
            }),
            Fake::NotFound => Err(FetchError::NotFound {
                url: "https://example.com/a".to_string(),
            }),
        }
    }
}

/// Config in a temp dir; graph off so the (stubbed) later stages stay inert.
fn fixture(dir: &Path, toml_body: &str) -> Arc<Config> {
    let toml_path = dir.join("ohara.toml");
    std::fs::write(
        &toml_path,
        format!("data_dir = {:?}\n{toml_body}", dir.join("data").display()),
    )
    .unwrap();
    Arc::new(Config::load(Some(&toml_path)).unwrap())
}

fn enqueue_url(conn: &rusqlite::Connection, data_dir: &Path, url: &str) -> String {
    control::insert_new(
        conn,
        data_dir,
        &control::NewDocument {
            source_url: url.to_string(),
            source_url_normalized: url.to_string(),
            priority: 5,
            pipeline_version: "0.1.0".to_string(),
        },
        NOW,
    )
    .unwrap()
    .doc_id()
    .to_string()
}

/// One tick on a blocking thread — the production shape for the fetch call.
async fn tick(worker: &Arc<Worker>) -> usize {
    let worker = Arc::clone(worker);
    tokio::task::spawn_blocking(move || worker.tick_once())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn worker_drives_a_document_from_new_to_cleaned() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    let config = fixture(dir.path(), "");
    let conn = control::connect(config.db_path()).unwrap();
    let doc_id = enqueue_url(&conn, &dir.path().join("data"), "https://example.com/a");
    let worker = Arc::new(
        Worker::with_ports(
            Arc::clone(&config),
            Arc::new(FakeFetcher {
                result: Fake::Html(ARTICLE_HTML),
            }),
            Arc::new(ohara::pipeline::ReadabilityExtractor),
        )
        .unwrap(),
    );

    // Tick 1: SCRAPE done — milestone SCRAPED, raw payload stored, CLEAN chained.
    assert_eq!(tick(&worker).await, 1);
    let doc = control::get(&conn, &doc_id).unwrap().unwrap();
    assert_eq!(doc.status, DocStatus::Scraped);
    assert!(Path::new(&doc.raw_file_path).is_file());
    assert!(
        Path::new(&doc.raw_file_path)
            .exists()
            .then_some(())
            .is_some()
    );
    let clean_jobs: i64 = conn
        .query_row(
            "SELECT count(*) FROM jobs WHERE doc_id = ?1 AND stage = 'CLEAN' AND status = 'PENDING'",
            [&doc_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        clean_jobs, 1,
        "§6: CLEAN chained in the completion transaction"
    );

    // Tick 2: CLEAN done — real extractor, sanitize, gates, clean file on disk.
    assert_eq!(tick(&worker).await, 1);
    let doc = control::get(&conn, &doc_id).unwrap().unwrap();
    assert_eq!(doc.status, DocStatus::Cleaned);
    let clean_path = doc.clean_file_path.clone().unwrap();
    assert!(Path::new(&clean_path).is_file());
    let markdown = std::fs::read_to_string(&clean_path).unwrap();
    assert!(
        markdown.contains("embedded database engine"),
        "real extractor output: {markdown:?}"
    );
    assert_eq!(doc.title.as_deref(), Some("On SQLite"));
    assert_eq!(doc.language.as_deref(), Some("en"));
    assert!(doc.word_count.unwrap_or(0) >= 50);

    // The chain queued VECTORIZE; the loop stopped before the stub stage.
    let vectorize: i64 = conn
        .query_row(
            "SELECT count(*) FROM jobs WHERE doc_id = ?1 AND stage = 'VECTORIZE' AND status = 'PENDING'",
            [&doc_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(vectorize, 1);

    // The full chain is auditable (§1.2.6).
    let done_events: i64 = conn
        .query_row(
            "SELECT count(*) FROM stage_events WHERE doc_id = ?1 AND outcome = 'DONE'",
            [&doc_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(done_events, 2, "SCRAPE and CLEAN audited");
}

#[tokio::test]
async fn quality_rejection_completes_without_chaining() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    let config = fixture(dir.path(), "");
    let conn = control::connect(config.db_path()).unwrap();
    let doc_id = enqueue_url(&conn, &dir.path().join("data"), "https://example.com/a");
    let worker = Arc::new(
        Worker::with_ports(
            Arc::clone(&config),
            Arc::new(FakeFetcher {
                result: Fake::Html("<html><body>hi</body></html>"),
            }),
            Arc::new(ohara::pipeline::ReadabilityExtractor),
        )
        .unwrap(),
    );

    tick(&worker).await; // SCRAPE
    tick(&worker).await; // CLEAN — quality gate rejects

    let doc = control::get(&conn, &doc_id).unwrap().unwrap();
    assert_eq!(doc.status, DocStatus::FailedQuality);
    assert!(doc.error.as_deref().unwrap_or("").contains("word count"));

    // Nothing chained: the pipeline stopped at the gate (§8 Stage 2).
    let pending: i64 = conn
        .query_row(
            "SELECT count(*) FROM jobs WHERE doc_id = ?1 AND status = 'PENDING'",
            [&doc_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pending, 0);
    let vectorize: i64 = conn
        .query_row(
            "SELECT count(*) FROM jobs WHERE doc_id = ?1 AND stage = 'VECTORIZE'",
            [&doc_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(vectorize, 0);
}

#[tokio::test]
async fn not_found_dead_job_fails_its_document() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    let config = fixture(dir.path(), "");
    let conn = control::connect(config.db_path()).unwrap();
    let doc_id = enqueue_url(&conn, &dir.path().join("data"), "https://example.com/a");
    let worker = Arc::new(
        Worker::with_ports(
            Arc::clone(&config),
            Arc::new(FakeFetcher {
                result: Fake::NotFound,
            }),
            Arc::new(ohara::pipeline::ReadabilityExtractor),
        )
        .unwrap(),
    );

    assert_eq!(tick(&worker).await, 1);

    let (job_status, doc_status): (String, String) = conn
        .query_row(
            "SELECT j.status, d.status FROM jobs j JOIN documents d USING (doc_id)
              WHERE d.doc_id = ?1",
            [&doc_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(job_status, "DEAD", "NotFound is Permanent (§10)");
    assert_eq!(doc_status, "FAILED", "§6 terminal mapping");
    let dead_events: i64 = conn
        .query_row(
            "SELECT count(*) FROM stage_events WHERE doc_id = ?1 AND outcome = 'DEAD'",
            [&doc_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(dead_events, 1);
}

/// An extractor that always fails with a Retry-class error (§10).
struct FailingExtractor;

impl Extractor for FailingExtractor {
    fn extract(&self, _html: &str, _base: &str) -> Result<ExtractedArticle, ExtractError> {
        Err(ExtractError::Failed("inference backend hiccup".to_string()))
    }
}

/// The extractor port contract stays honest through the worker too: a failed
/// extractor (Retry class) retries, it does not dead the document on the spot.
#[tokio::test]
async fn extractor_failure_retries_via_backoff() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    // Short backoff so the retry lands quickly.
    let config = fixture(dir.path(), "backoff_base_secs = 1\n");
    let conn = control::connect(config.db_path()).unwrap();
    let doc_id = enqueue_url(&conn, &dir.path().join("data"), "https://example.com/a");

    let worker = Arc::new(
        Worker::with_ports(
            Arc::clone(&config),
            Arc::new(FakeFetcher {
                result: Fake::Html(ARTICLE_HTML),
            }),
            Arc::new(FailingExtractor),
        )
        .unwrap(),
    );

    tick(&worker).await; // SCRAPE
    tick(&worker).await; // CLEAN — fails once, back to PENDING

    let doc = control::get(&conn, &doc_id).unwrap().unwrap();
    assert_eq!(doc.status, DocStatus::Scraped, "milestone not reached");
    assert_eq!(
        doc.error.as_deref(),
        None,
        "the failure lives on the job, not the doc"
    );
    let (status, attempts, last_error): (String, i64, Option<String>) = conn
        .query_row(
            "SELECT status, attempts, last_error FROM jobs
              WHERE doc_id = ?1 AND stage = 'CLEAN'",
            [&doc_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(status, "PENDING");
    assert_eq!(attempts, 1, "attempts count ended executions (§6)");
    assert_eq!(
        last_error.as_deref(),
        Some("transient failure (attempt 1): extractor failed: inference backend hiccup")
    );

    // The retry event is audited.
    let retries: i64 = conn
        .query_row(
            "SELECT count(*) FROM stage_events WHERE doc_id = ?1 AND outcome = 'RETRY'",
            [&doc_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(retries, 1);
}
