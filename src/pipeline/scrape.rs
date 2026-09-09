//! Stage 1 — Scrape (§8): politeness + robots via the [`Fetcher`] port → raw
//! payload → `data/raw/<doc_id>.html.gz` → fetch metadata on the document row.
//! The milestone (`SCRAPED`) and the CLEAN chaining happen in the completion
//! transaction, not here. Ladder escalation (legs 2–3) arrives with §15 step 7.

use std::time::Duration;

use crate::Class;
use crate::control::{self, ClaimedJob};
use crate::engine::{FetchError, FetchPolicy, FetchValidators, NormalizedUrl};

use super::execution::ScrapeContext;
use super::{StageError, StageOutcome};

/// Runs the stage for one claimed job.
///
/// # Errors
/// [`StageError`] classified per the §10 mapping: `NotFound` is permanent; every
/// other fetch failure retries within the attempt budget; store failures are
/// fatal.
pub(super) fn run(ctx: &ScrapeContext<'_>, job: &ClaimedJob) -> Result<StageOutcome, StageError> {
    let doc = control::get(ctx.conn, job.doc_id())?
        .ok_or_else(|| StageError::fatal("claimed job's document row is missing"))?;
    let now = control::now();
    let url =
        NormalizedUrl::parse(&doc.source_url_normalized).map_err(|e| StageError::Permanent {
            reason: format!("stored normalized URL is invalid: {e}"),
        })?;

    // Per-host policy (§5 sites): the rate override is a floor on top of the
    // fetcher's own default; the ladder hint guides escalation in §15 step 7.
    let site = control::site_policy(ctx.conn, url.host_str())?;
    let policy = FetchPolicy {
        rate_limit: site
            .as_ref()
            .and_then(|s| s.rate_limit_ms)
            .and_then(|ms| u64::try_from(ms).ok())
            .map_or(Duration::ZERO, Duration::from_millis),
        robots: ctx.config.fetcher().robots(),
    };

    let validators = if std::path::Path::new(&doc.raw_file_path).is_file() {
        FetchValidators {
            etag: doc.etag.clone(),
            last_modified: doc.last_modified.clone(),
        }
    } else {
        FetchValidators::default()
    };
    let fetched = ctx
        .handle
        .block_on(
            ctx.fetcher
                .fetch_with_validators(&url, &policy, &validators),
        )
        .map_err(|e| map_fetch_error(e, job))?;

    let recrawl_at = site
        .as_ref()
        .and_then(|policy| policy.recrawl_seconds)
        .and_then(|seconds| u64::try_from(seconds).ok())
        .filter(|seconds| *seconds > 0)
        .map(|seconds| {
            let interval = if fetched.status == 304 {
                seconds.saturating_mul(2)
            } else {
                seconds
            };
            control::now_plus(
                &now,
                interval.min(ctx.config.fetcher().max_recrawl_seconds()),
            )
        })
        .transpose()?;
    if fetched.status == 304 {
        if !std::path::Path::new(&doc.raw_file_path).is_file() {
            return Err(StageError::Permanent {
                reason: "server returned 304 but the stored raw payload is missing".to_string(),
            });
        }
        control::update_fetch_result(
            ctx.conn,
            job.doc_id(),
            i64::from(fetched.status),
            fetched.etag.as_deref(),
            fetched.last_modified.as_deref(),
            recrawl_at.as_deref(),
            &now,
        )?;
        return Ok(StageOutcome::Stop);
    }
    write_gz(&doc.raw_file_path, fetched.html.as_bytes(), attempt_of(job))?;
    control::update_fetch_result(
        ctx.conn,
        job.doc_id(),
        i64::from(fetched.status),
        fetched.etag.as_deref(),
        fetched.last_modified.as_deref(),
        recrawl_at.as_deref(),
        &now,
    )?;
    Ok(StageOutcome::Advance)
}

/// The §10 mapping for fetch failures: `NotFound` cannot succeed on retry; the
/// rest retry within the attempt budget with their own class recorded.
fn map_fetch_error(error: FetchError, job: &ClaimedJob) -> StageError {
    let attempt = attempt_of(job);
    match error.class() {
        Class::Permanent => StageError::Permanent {
            reason: error.to_string(),
        },
        Class::Retry => StageError::transient(error, Class::Retry, attempt),
        Class::Fatal => StageError::fatal(error),
    }
}

/// The ended-execution count of this run, 1-based (§6: attempts count ended
/// executions) — carried on transient errors for the audit trail.
fn attempt_of(job: &ClaimedJob) -> u32 {
    u32::try_from(job.attempts() + 1).unwrap_or(u32::MAX)
}

/// Writes the raw payload as gzip (§3: `data/raw/<doc_id>.html.gz`), creating
/// the `raw/` directory on first use. Local I/O is treated as transient (disk
/// pressure clears; §10).
fn write_gz(path: &str, payload: &[u8], attempt: u32) -> Result<(), StageError> {
    let io_err = |e: std::io::Error| StageError::transient(e, Class::Retry, attempt);
    let path = std::path::Path::new(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io_err)?;
    }
    let file = std::fs::File::create(path).map_err(io_err)?;
    let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    std::io::Write::write_all(&mut encoder, payload)
        .and_then(|()| encoder.finish())
        .map_err(io_err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // §10: tests unwrap freely

    use std::sync::Arc;

    use crate::config::Config;
    use crate::control::testing::seed_doc;
    use crate::control::{self, Completion, DocStatus, Stage};
    use crate::engine::{
        FetchCapabilities, FetchError, FetchPolicy, FetchedDoc, Fetcher, NormalizedUrl,
    };

    use super::super::execution::ScrapeContext;
    use super::{StageError, StageOutcome, run};

    const NOW: &str = "2026-09-06 12:00:00";

    /// The fetcher port fake (§14): serves a canned document or a canned error.
    enum Fake {
        Doc(FetchedDoc),
        NotModified(FetchedDoc),
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
                Fake::Doc(doc) | Fake::NotModified(doc) => Ok(doc.clone()),
                Fake::NotFound => Err(FetchError::NotFound {
                    url: "https://example.com/a".to_string(),
                }),
            }
        }
    }

    fn fixture_config(dir: &std::path::Path) -> Arc<Config> {
        let toml_path = dir.join("ohara.toml");
        std::fs::write(
            &toml_path,
            format!("data_dir = {:?}\n", dir.join("data").display()),
        )
        .unwrap();
        Arc::new(Config::load(Some(&toml_path)).unwrap())
    }

    fn fetched_doc() -> FetchedDoc {
        FetchedDoc {
            html: "<html><body>the page</body></html>".to_string(),
            js_executed: false,
            final_url: "https://example.com/a".to_string(),
            status: 200,
            content_type: Some("text/html; charset=utf-8".to_string()),
            etag: Some("\"v1\"".to_string()),
            last_modified: Some("Fri, 04 Sep 2026 00:00:00 GMT".to_string()),
            fetched_at: "2026-09-06T12:00:00+00:00".to_string(),
        }
    }

    #[tokio::test]
    async fn fetched_payload_is_stored_and_documented() {
        let dir = tempfile::tempdir().unwrap();
        let config = fixture_config(dir.path());
        let store = dir.path().join("store.db");
        let conn = control::connect(&store).unwrap();
        let doc_id = seed_doc(&conn, "a");
        let fetcher = FakeFetcher {
            result: Fake::Doc(fetched_doc()),
        };
        // seed_doc's insert_new already created the PENDING SCRAPE job (§6).
        let job = control::claim_next(&conn, Stage::Scrape, "w1", NOW, 60)
            .unwrap()
            .unwrap();
        let run_job = job.clone();

        // The stage runs from a blocking thread with its store connection, the
        // production shape (the tick holds the connection in `spawn_blocking`).
        let outcome = tokio::task::spawn_blocking({
            let config = Arc::clone(&config);
            move || {
                let handle = tokio::runtime::Handle::current();
                let ctx = ScrapeContext {
                    config: config.as_ref(),
                    conn: &conn,
                    handle: &handle,
                    fetcher: &fetcher,
                };
                run(&ctx, &run_job)
            }
        })
        .await
        .unwrap()
        .unwrap();
        let conn = control::connect(&store).unwrap();

        assert_eq!(outcome, StageOutcome::Advance);
        let doc = control::get(&conn, &doc_id).unwrap().unwrap();
        assert!(std::path::Path::new(&doc.raw_file_path).is_file());
        // The payload round-trips through gzip (§3).
        let file = std::fs::File::open(&doc.raw_file_path).unwrap();
        let mut decoder = flate2::read::GzDecoder::new(file);
        let mut html = String::new();
        std::io::Read::read_to_string(&mut decoder, &mut html).unwrap();
        assert!(html.contains("the page"));
        assert_eq!(doc.http_status, Some(200));
        assert_eq!(doc.etag.as_deref(), Some("\"v1\""));
        assert_eq!(
            doc.fetched_at.as_deref().map_or(0, str::len),
            19,
            "§5 timestamp format (UTC YYYY-MM-DD HH:MM:SS)"
        );

        control::complete(&conn, Stage::Scrape, &job, Completion::Chain, NOW).unwrap();
        assert_eq!(
            control::get(&conn, &doc_id).unwrap().unwrap().status,
            DocStatus::Scraped
        );
    }

    #[tokio::test]
    async fn not_found_is_a_permanent_failure() {
        let dir = tempfile::tempdir().unwrap();
        let config = fixture_config(dir.path());
        let store = dir.path().join("store.db");
        let conn = control::connect(&store).unwrap();
        let doc_id = seed_doc(&conn, "a");
        let fetcher = FakeFetcher {
            result: Fake::NotFound,
        };
        // seed_doc's insert_new already created the PENDING SCRAPE job (§6).
        let job = control::claim_next(&conn, Stage::Scrape, "w1", NOW, 60)
            .unwrap()
            .unwrap();
        let run_job = job.clone();

        let err = tokio::task::spawn_blocking({
            let config = Arc::clone(&config);
            move || {
                let handle = tokio::runtime::Handle::current();
                let ctx = ScrapeContext {
                    config: config.as_ref(),
                    conn: &conn,
                    handle: &handle,
                    fetcher: &fetcher,
                };
                run(&ctx, &run_job)
            }
        })
        .await
        .unwrap()
        .unwrap_err();
        let conn = control::connect(&store).unwrap();

        assert!(matches!(err, StageError::Permanent { .. }), "got {err:?}");
        // §6 terminal mapping: DEAD job ⇒ FAILED document.
        control::dead(&conn, job.job_id(), job.doc_id(), &err.to_string(), NOW).unwrap();
        assert_eq!(
            control::get(&conn, &doc_id).unwrap().unwrap().status,
            DocStatus::Failed
        );
    }

    #[tokio::test]
    async fn not_modified_stops_the_chain_and_schedules_the_next_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let config = fixture_config(dir.path());
        let store = dir.path().join("store.db");
        let conn = control::connect(&store).unwrap();
        let doc_id = seed_doc(&conn, "a");
        control::set_site_policy(&conn, "example.com", None, Some(3_600), None).unwrap();
        let raw_path = control::get(&conn, &doc_id).unwrap().unwrap().raw_file_path;
        let parent = std::path::Path::new(&raw_path).parent().unwrap();
        std::fs::create_dir_all(parent).unwrap();
        std::fs::write(&raw_path, b"previous raw payload").unwrap();
        let mut unchanged = fetched_doc();
        unchanged.html.clear();
        unchanged.status = 304;
        let fetcher = FakeFetcher {
            result: Fake::NotModified(unchanged),
        };
        let job = control::claim_next(&conn, Stage::Scrape, "w1", NOW, 60)
            .unwrap()
            .unwrap();
        let run_job = job.clone();

        let outcome = tokio::task::spawn_blocking({
            let config = Arc::clone(&config);
            move || {
                let handle = tokio::runtime::Handle::current();
                let ctx = ScrapeContext {
                    config: config.as_ref(),
                    conn: &conn,
                    handle: &handle,
                    fetcher: &fetcher,
                };
                run(&ctx, &run_job)
            }
        })
        .await
        .unwrap()
        .unwrap();
        let conn = control::connect(&store).unwrap();

        assert_eq!(outcome, StageOutcome::Stop);
        let doc = control::get(&conn, &doc_id).unwrap().unwrap();
        assert_eq!(doc.http_status, Some(304));
        assert!(doc.next_crawl_at.is_some());
        assert_eq!(doc.status, DocStatus::New);
    }
}
