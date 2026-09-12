//! Integration (§14): the worker loop end-to-end with injected ports — a canned
//! [`Fetcher`] fake and the real readability extractor drive a document from
//! `NEW` through `SCRAPED` to `CLEANED` through the real §6 machinery
//! ([`ohara::pipeline::Worker::tick_once`]).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // §10: tests unwrap freely

use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use ohara::config::Config;
use ohara::control::{self, ControlDb, DocStatus, NewChunkRow};
use ohara::engine::{
    FetchCapabilities, FetchError, FetchPolicy, FetchedDoc, Fetcher, NormalizedUrl,
};
use ohara::knowledge::{
    EntityRecord, EntityType, KnowledgeError, KnowledgeStore, ModelId, ScoredHit, VectorSpace,
};
use ohara::pipeline::{EmbedError, Embedder, ExtractError, ExtractedArticle, Extractor, Worker};

const NOW: &str = "2026-09-06 12:00:00";

/// A small but complete article: passes every §8 Stage 2 gate.
const ARTICLE_HTML: &str = r"<html><head><title>On SQLite</title></head><body>
    <article><p>SQLite is an embedded database engine that stores data in a single
    cross-platform file and needs no separate server process at all. Developers love
    it because the deployment story could hardly be simpler for applications of
    almost every conceivable shape and size. The library reads and writes directly
    to ordinary disk files, and the complete database with multiple tables, indices,
    triggers, and views lives inside one portable file.</p></article></body></html>";

const PAYWALL_HTML: &str = r"<html><body><article><p>SQLite is an embedded database engine
that stores data in a single cross-platform file and needs no separate server process.
The library supports tables, indexes, triggers, and views while keeping deployment
simple for applications of many shapes and sizes. It is widely used on phones,
browsers, and desktop tools because the complete database remains portable and easy
to back up. Subscribe to continue reading.</p></article></body></html>";

const FRENCH_HTML: &str = r"<html><body><article><p>Le système de gestion de base de données
relationnelle permet de stocker des informations structurées dans des tables reliées
entre elles par des clés étrangères très pratiques pour les applications modernes et
anciennes. Les développeurs peuvent créer des index, des déclencheurs et des vues
pour organiser les données, améliorer les recherches et conserver une architecture
simple, portable et fiable sur plusieurs plateformes et environnements.</p></article></body></html>";

/// The fetcher port fake (§14).
enum Fake {
    Html(&'static str),
    NotFound,
}

struct FakeFetcher {
    result: Fake,
}

/// A fetcher sequence that makes the worker exercise retry, terminal failure,
/// and recovery through the real queue state machine (§6).
struct RetryThenPermanentFetcher {
    calls: AtomicUsize,
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

#[async_trait::async_trait]
impl Fetcher for RetryThenPermanentFetcher {
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
        match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => Err(FetchError::Timeout { secs: 1 }),
            1 => Err(FetchError::NotFound {
                url: "https://example.com/recovery".to_string(),
            }),
            _ => Ok(FetchedDoc {
                html: ARTICLE_HTML.to_string(),
                js_executed: false,
                final_url: "https://example.com/recovery".to_string(),
                status: 200,
                content_type: Some("text/html; charset=utf-8".to_string()),
                etag: None,
                last_modified: None,
                fetched_at: "2026-09-06T12:00:00+00:00".to_string(),
            }),
        }
    }
}

/// The embedder port fake (§14): deterministic vectors, whitespace token
/// counting +2 (specials). Same shape as the §9 contract requires of the real
/// ONNX embedder: pinned model id, fixed dim, order-preserving batch.
struct FixedEmbedder;

impl Embedder for FixedEmbedder {
    fn model_id(&self) -> &'static str {
        "fake-embedder"
    }

    fn dim(&self) -> usize {
        4
    }

    fn max_input_tokens(&self) -> usize {
        512
    }

    fn count_tokens(&self, text: &str) -> usize {
        text.split_whitespace().count() + 2
    }

    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(texts
            .iter()
            .map(|t| {
                let seed = f32::from(t.as_bytes().first().copied().unwrap_or(b' '));
                vec![seed % 8.0 + 1.0, seed % 5.0 + 1.0, 1.0, 0.5]
            })
            .collect())
    }
}

/// Provider fake with a smaller input capacity than the default chunk budget.
struct LimitedCapacityEmbedder;

impl Embedder for LimitedCapacityEmbedder {
    fn model_id(&self) -> &'static str {
        "fake-embedder"
    }

    fn dim(&self) -> usize {
        4
    }

    fn max_input_tokens(&self) -> usize {
        128
    }

    fn count_tokens(&self, text: &str) -> usize {
        text.split_whitespace().count() + 2
    }

    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        FixedEmbedder.embed(texts)
    }
}

/// The production port shape, backed by a deterministic in-memory store for
/// hermetic integration tests.
fn memory_knowledge() -> Arc<dyn KnowledgeStore> {
    Arc::new(ohara::knowledge::InMemoryKnowledge::default())
}

/// Config in a temp dir; graph off so the (stubbed) later stages stay inert.
fn fixture(dir: &Path, toml_body: &str) -> Arc<Config> {
    let toml_path = dir.join("ohara.toml");
    std::fs::write(
        &toml_path,
        format!(
            "data_dir = {:?}\n\
{toml_body}\
[embedder]\n\
model_id = \"fake-embedder\"\n\
dim = 4\n\
[knowledge]\n\
read_model = \"fake-embedder\"\n\
write_model = \"fake-embedder\"\n",
            dir.join("data").display()
        ),
    )
    .unwrap();
    Arc::new(Config::load(Some(&toml_path)).unwrap())
}

#[tokio::test]
async fn worker_rejects_embedder_namespace_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    let toml_path = dir.path().join("ohara.toml");
    std::fs::write(
        &toml_path,
        format!(
            "data_dir = {:?}\n[embedder]\nmodel_id = \"configured-model\"\ndim = 4\n[knowledge]\nread_model = \"configured-model\"\nwrite_model = \"configured-model\"\n[pipeline]\ngraph_enabled = false\n",
            dir.path().join("data").display()
        ),
    )
    .unwrap();
    let config = Arc::new(Config::load(Some(&toml_path)).unwrap());

    let result = Worker::with_ports(
        config,
        Arc::new(FakeFetcher {
            result: Fake::Html(ARTICLE_HTML),
        }),
        Arc::new(ohara::pipeline::ReadabilityExtractor),
        Arc::new(FixedEmbedder),
        memory_knowledge(),
        Arc::new(ohara::llm::NoLlm),
    );
    assert!(matches!(
        result,
        Err(ohara::BootError::Worker(message)) if message.contains("embedder.model_id")
    ));
}

#[tokio::test]
async fn worker_rejects_chunk_budget_above_embedder_capacity() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    let config = fixture(
        dir.path(),
        "[pipeline]\nchunk_budget_tokens = 512\ngraph_enabled = false\n",
    );

    let result = Worker::with_ports(
        config,
        Arc::new(FakeFetcher {
            result: Fake::Html(ARTICLE_HTML),
        }),
        Arc::new(ohara::pipeline::ReadabilityExtractor),
        Arc::new(LimitedCapacityEmbedder),
        memory_knowledge(),
        Arc::new(ohara::llm::NoLlm),
    );
    assert!(matches!(
        result,
        Err(ohara::BootError::Worker(message))
            if message.contains("chunk budget") && message.contains("max input tokens")
    ));
}

fn enqueue_url(conn: &ControlDb, data_dir: &Path, url: &str) -> String {
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
            Arc::new(FixedEmbedder),
            memory_knowledge(),
            Arc::new(ohara::llm::NoLlm),
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
    // The next stage is observable through the public claim port; it must be
    // chained by the completion transaction.
    assert!(
        control::claim_next(&conn, ohara::control::Stage::Vectorize, "probe", NOW, 60)
            .unwrap()
            .is_some(),
        "VECTORIZE must be chained after CLEAN"
    );
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
            Arc::new(FixedEmbedder),
            memory_knowledge(),
            Arc::new(ohara::llm::NoLlm),
        )
        .unwrap(),
    );

    tick(&worker).await; // SCRAPE
    tick(&worker).await; // CLEAN — quality gate rejects

    let doc = control::get(&conn, &doc_id).unwrap().unwrap();
    assert_eq!(doc.status, DocStatus::FailedQuality);
    assert!(doc.error.as_deref().unwrap_or("").contains("word count"));

    // Nothing chained: the pipeline stopped at the gate (§8 Stage 2).
    assert!(
        control::claim_next(&conn, ohara::control::Stage::Vectorize, "probe", NOW, 60)
            .unwrap()
            .is_none(),
        "quality rejection must not chain VECTORIZE"
    );
}

async fn assert_quality_rejection(html: &'static str, reason: &str) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    let config = fixture(dir.path(), "");
    let conn = control::connect(config.db_path()).unwrap();
    let doc_id = enqueue_url(
        &conn,
        &dir.path().join("data"),
        "https://example.com/quality",
    );
    let worker = Arc::new(
        Worker::with_ports(
            Arc::clone(&config),
            Arc::new(FakeFetcher {
                result: Fake::Html(html),
            }),
            Arc::new(ohara::pipeline::ReadabilityExtractor),
            Arc::new(FixedEmbedder),
            memory_knowledge(),
            Arc::new(ohara::llm::NoLlm),
        )
        .unwrap(),
    );

    tick(&worker).await; // SCRAPE
    tick(&worker).await; // CLEAN — quality gate rejects

    let doc = control::get(&conn, &doc_id).unwrap().unwrap();
    assert_eq!(doc.status, DocStatus::FailedQuality);
    assert!(
        doc.error.as_deref().unwrap_or("").contains(reason),
        "quality rejection must explain {reason}: {doc:?}"
    );
    assert!(
        control::claim_next(&conn, ohara::control::Stage::Vectorize, "probe", NOW, 60)
            .unwrap()
            .is_none(),
        "quality rejection must not chain VECTORIZE"
    );
}

#[tokio::test]
async fn quality_evaluation_covers_paywall_and_wrong_language() {
    assert_quality_rejection(PAYWALL_HTML, "paywall").await;
    assert_quality_rejection(FRENCH_HTML, "language").await;
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
            Arc::new(FixedEmbedder),
            memory_knowledge(),
            Arc::new(ohara::llm::NoLlm),
        )
        .unwrap(),
    );

    assert_eq!(tick(&worker).await, 1);

    assert_eq!(
        control::get(&conn, &doc_id).unwrap().unwrap().status,
        DocStatus::Failed,
        "NotFound is Permanent (§10)"
    );
}

/// The worker-level recovery path (§6): a transient fetch is retried, a later
/// permanent classification dead-letters the job, and `requeue` grants a fresh
/// budget so the same worker can complete the document.
#[tokio::test]
async fn retry_then_dead_letter_requeues_and_recovers() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    let config = fixture(
        dir.path(),
        "backoff_base_secs = 1\n[pipeline]\ngraph_enabled = false\n",
    );
    let conn = control::connect(config.db_path()).unwrap();
    let doc_id = enqueue_url(
        &conn,
        &dir.path().join("data"),
        "https://example.com/recovery",
    );
    let worker = Arc::new(
        Worker::with_ports(
            Arc::clone(&config),
            Arc::new(RetryThenPermanentFetcher {
                calls: AtomicUsize::new(0),
            }),
            Arc::new(ohara::pipeline::ReadabilityExtractor),
            Arc::new(FixedEmbedder),
            memory_knowledge(),
            Arc::new(ohara::llm::NoLlm),
        )
        .unwrap(),
    );

    // The first transient failure is pending behind the configured backoff.
    assert_eq!(tick(&worker).await, 1);
    assert_eq!(
        control::get(&conn, &doc_id).unwrap().unwrap().status,
        DocStatus::New
    );

    // The timestamp format is second-precision, so two seconds clears the
    // one-second test backoff even when the first tick is near a boundary.
    tokio::time::sleep(Duration::from_secs(2)).await;

    // The second attempt is permanently not found and must become DEAD.
    assert_eq!(tick(&worker).await, 1);
    assert_eq!(
        control::get(&conn, &doc_id).unwrap().unwrap().status,
        DocStatus::Failed
    );
    let events = control::events_for_doc(&conn, &doc_id).unwrap();
    let outcomes: Vec<&str> = events.iter().map(|event| event.outcome.as_str()).collect();
    assert_eq!(outcomes, ["RETRY", "DEAD"]);
    assert!(
        events[0]
            .detail
            .as_deref()
            .unwrap_or("")
            .contains("timeout")
    );
    assert!(
        events[1]
            .detail
            .as_deref()
            .unwrap_or("")
            .contains("not found")
    );

    // Recovery is an explicit operator action: reset the dead SCRAPE job, then
    // let the real worker continue through CLEAN and VECTORIZE.
    assert_eq!(control::requeue(&conn, &doc_id, NOW).unwrap(), 1);
    assert_eq!(tick(&worker).await, 1); // SCRAPE succeeds on the third fetch.
    assert_eq!(tick(&worker).await, 1); // CLEAN
    assert_eq!(tick(&worker).await, 1); // VECTORIZE

    assert_eq!(
        control::get(&conn, &doc_id).unwrap().unwrap().status,
        DocStatus::Vectorized
    );
    assert!(
        control::claim_next(&conn, ohara::control::Stage::Extract, "probe", NOW, 60)
            .unwrap()
            .is_none(),
        "graph-disabled recovery ends at VECTORIZE"
    );
}

/// A restart acceptance case for the cross-store deletion protocol (§7.6): the
/// `SQLite` intent survives the first process, and the next worker removes the
/// knowledge index before completing the `SQLite` cascade.
#[tokio::test]
async fn worker_restart_reconciles_deletion_across_stores() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    let config = fixture(dir.path(), "");
    let conn = control::connect(config.db_path()).unwrap();
    let doc_id = enqueue_url(
        &conn,
        &dir.path().join("data"),
        "https://example.com/doomed",
    );
    let chunk_id = "chunk-doomed";
    let model = VectorSpace::Chunks {
        model_id: ModelId::new("fake-embedder"),
    };
    control::replace_chunks(
        &conn,
        &doc_id,
        &[NewChunkRow {
            chunk_id: chunk_id.to_string(),
            seq: 0,
            header_path: "article".to_string(),
            text: "SQLite survives process restarts".to_string(),
            embed_text: "SQLite survives process restarts".to_string(),
            token_count: 5,
            embedding_model: "fake-embedder".to_string(),
            content_hash: "hash-doomed".to_string(),
        }],
    )
    .unwrap();

    // Populate a knowledge store, then close the first worker to model the
    // process exiting after it recorded the deletion intent.
    let shared_store = Arc::new(ohara::knowledge::InMemoryKnowledge::default());
    let first_store = Arc::clone(&shared_store);
    first_store
        .upsert_vectors(
            model.clone(),
            &doc_id,
            &[chunk_id],
            &[vec![1.0, 0.0, 0.0, 0.0]],
        )
        .await
        .unwrap();
    first_store
        .upsert_entity(&EntityRecord {
            entity_id: "entity-doomed".to_string(),
            canonical_name: "SQLite".to_string(),
            entity_type: EntityType::Product,
            subtype: None,
        })
        .await
        .unwrap();
    first_store
        .link_mention(chunk_id, "entity-doomed")
        .await
        .unwrap();
    assert!(
        first_store
            .has_vector(model.clone(), chunk_id)
            .await
            .unwrap()
    );
    drop(first_store);

    control::request_deletion(&conn, &doc_id, Some("restart acceptance")).unwrap();

    // The second worker is the boot/restart boundary under test.
    let restarted_store = Arc::clone(&shared_store);
    let worker = Worker::with_ports(
        Arc::clone(&config),
        Arc::new(FakeFetcher {
            result: Fake::NotFound,
        }),
        Arc::new(ohara::pipeline::ReadabilityExtractor),
        Arc::new(FixedEmbedder),
        restarted_store.clone(),
        Arc::new(ohara::llm::NoLlm),
    )
    .unwrap();

    let report = worker.reconcile().await.unwrap();
    assert_eq!(report.deletions_executed, 1);
    assert!(control::get(&conn, &doc_id).unwrap().is_none());
    assert!(control::pending_deletions(&conn).unwrap().is_empty());
    assert!(!restarted_store.has_vector(model, chunk_id).await.unwrap());
    assert!(
        restarted_store
            .chunks_for_entities(&["entity-doomed"])
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        control::search_bm25(&conn, "survives", 10)
            .unwrap()
            .is_empty()
    );

    let again = worker.reconcile().await.unwrap();
    assert_eq!(
        again.deletions_executed, 0,
        "restart reconciliation is idempotent"
    );
}

/// A restart acceptance case for the lease protocol (§6): a process dies after
/// claiming CLEAN, and the next worker reclaims the expired lease exactly once
/// before continuing the stage chain.
#[tokio::test]
async fn worker_restart_reclaims_expired_lease() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    let config = fixture(dir.path(), "");
    let conn = control::connect(config.db_path()).unwrap();
    let doc_id = enqueue_url(
        &conn,
        &dir.path().join("data"),
        "https://example.com/restart",
    );
    let first_worker = Arc::new(
        Worker::with_ports(
            Arc::clone(&config),
            Arc::new(FakeFetcher {
                result: Fake::Html(ARTICLE_HTML),
            }),
            Arc::new(ohara::pipeline::ReadabilityExtractor),
            Arc::new(FixedEmbedder),
            memory_knowledge(),
            Arc::new(ohara::llm::NoLlm),
        )
        .unwrap(),
    );

    // SCRAPE completes normally and chains CLEAN. The simulated crashed
    // process claims CLEAN with a lease that is already expired by real time.
    assert_eq!(tick(&first_worker).await, 1);
    let crashed_claim = control::claim_next(
        &conn,
        ohara::control::Stage::Clean,
        "crashed-worker",
        "2000-01-01 00:00:00",
        1,
    )
    .unwrap()
    .expect("the simulated process claimed CLEAN before it crashed");
    assert_eq!(
        crashed_claim.attempts(),
        0,
        "a fresh claim is not an attempt"
    );
    drop(first_worker);

    let restarted_worker = Arc::new(
        Worker::with_ports(
            Arc::clone(&config),
            Arc::new(FakeFetcher {
                result: Fake::NotFound,
            }),
            Arc::new(ohara::pipeline::ReadabilityExtractor),
            Arc::new(FixedEmbedder),
            memory_knowledge(),
            Arc::new(ohara::llm::NoLlm),
        )
        .unwrap(),
    );
    assert_eq!(tick(&restarted_worker).await, 1);
    assert_eq!(
        control::get(&conn, &doc_id).unwrap().unwrap().status,
        DocStatus::Cleaned,
        "the restarted worker reclaimed CLEAN and advanced the milestone"
    );
    assert!(
        control::claim_next(&conn, ohara::control::Stage::Vectorize, "probe", NOW, 60,)
            .unwrap()
            .is_some(),
        "reclaimed completion must chain VECTORIZE"
    );
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
            Arc::new(FixedEmbedder),
            memory_knowledge(),
            Arc::new(ohara::llm::NoLlm),
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
    // The failed stage leaves the document at its last milestone and does not
    // advance it; the retry remains an implementation detail of the queue.
}

/// The §15 step 4 milestone: the worker drives a document all the way to
/// `VECTORIZED` — chunk rows in the registry, vectors in the (real, in-memory)
/// knowledge store, FTS index synced by trigger.
#[tokio::test]
async fn worker_drives_a_document_from_new_to_vectorized() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    // Graph off: §15 step 5 precedes Stage 4, so the chain must end at VECTORIZE.
    let config = fixture(dir.path(), "[pipeline]\ngraph_enabled = false\n");
    let conn = control::connect(config.db_path()).unwrap();
    let doc_id = enqueue_url(&conn, &dir.path().join("data"), "https://example.com/a");
    let knowledge = memory_knowledge();
    let worker = Arc::new(
        Worker::with_ports(
            Arc::clone(&config),
            Arc::new(FakeFetcher {
                result: Fake::Html(ARTICLE_HTML),
            }),
            Arc::new(ohara::pipeline::ReadabilityExtractor),
            Arc::new(FixedEmbedder),
            Arc::clone(&knowledge),
            Arc::new(ohara::llm::NoLlm),
        )
        .unwrap(),
    );

    tick(&worker).await; // SCRAPE
    tick(&worker).await; // CLEAN
    tick(&worker).await; // VECTORIZE

    let doc = control::get(&conn, &doc_id).unwrap().unwrap();
    assert_eq!(doc.status, DocStatus::Vectorized);
    assert_eq!(doc.chunk_count, 1, "single short article = one chunk");
    assert!(doc.token_count.unwrap_or(0) > 0);

    // Registry rows point at the embedder's model; the vector is present under
    // the same model's collection (§4 namespace discipline).
    let signature = control::chunk_signatures(&conn, &doc_id)
        .unwrap()
        .into_iter()
        .next()
        .expect("vectorized document has a chunk");
    let chunk_id = signature.chunk_id;
    let model = "fake-embedder";
    let space = VectorSpace::Chunks {
        model_id: ohara::knowledge::ModelId::new(model),
    };
    assert!(
        knowledge
            .has_vector(space.clone(), &chunk_id)
            .await
            .unwrap(),
        "chunk vector must exist after VECTORIZE"
    );

    // FTS is trigger-synced with the registry (§5): the chunk text is searchable.
    assert!(!control::search_bm25(&conn, "sqlite", 5).unwrap().is_empty());

    // graph_enabled=false ends the chain at VECTORIZE (§6): EXTRACT is not
    // claimable after VECTORIZE.
    assert!(
        control::claim_next(&conn, ohara::control::Stage::Extract, "probe", NOW, 60)
            .unwrap()
            .is_none()
    );
    let _ = KnowledgeError::Backend("witness".to_string());
    let _ = ScoredHit {
        id: String::new(),
        score: 0.0,
    };
}

/// The §15 step 6 milestone: the worker drives a document all the way to
/// `INDEXED` — Stage 4 runs end to end (LLM fake, real knowledge port): triplets
/// staged, entities + aliases registered, `:MENTIONS` linked, fact edges merged,
/// the §6 chain terminating at EXTRACT with no successor.
#[tokio::test]
async fn worker_drives_a_document_from_new_to_indexed() {
    // The scripted extractor: one valid triplet for the article's single chunk.
    struct ScriptedLlm;
    #[async_trait::async_trait]
    impl ohara::llm::Llm for ScriptedLlm {
        fn provider_name(&self) -> &'static str {
            "scripted"
        }

        async fn complete(
            &self,
            _req: ohara::llm::CompletionRequest,
        ) -> Result<ohara::llm::CompletionResponse, ohara::llm::LlmError> {
            Ok(ohara::llm::CompletionResponse {
                text: r#"{"triplets":[{"subject":"SQLite","subject_type":"PRODUCT",
                     "predicate":"CREATED_BY","object":"D. Richard Hipp",
                     "object_type":"PERSON"}]}"#
                    .to_string(),
                prompt_tokens: 40,
                completion_tokens: 30,
            })
        }
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    // Graph on: the chain continues past VECTORIZE into EXTRACT (§6).
    let config = fixture(dir.path(), "");
    let conn = control::connect(config.db_path()).unwrap();
    let doc_id = enqueue_url(&conn, &dir.path().join("data"), "https://example.com/a");
    let knowledge = memory_knowledge();

    let worker = Arc::new(
        Worker::with_ports(
            Arc::clone(&config),
            Arc::new(FakeFetcher {
                result: Fake::Html(ARTICLE_HTML),
            }),
            Arc::new(ohara::pipeline::ReadabilityExtractor),
            Arc::new(FixedEmbedder),
            Arc::clone(&knowledge),
            Arc::new(ScriptedLlm),
        )
        .unwrap(),
    );

    tick(&worker).await; // SCRAPE
    tick(&worker).await; // CLEAN
    tick(&worker).await; // VECTORIZE
    tick(&worker).await; // EXTRACT

    let doc = control::get(&conn, &doc_id).unwrap().unwrap();
    assert_eq!(doc.status, DocStatus::Indexed, "the full chain ran");

    // Stage 4's registry rows: the triplet cost cache and both entities.
    assert_eq!(control::triplets_of_doc(&conn, &doc_id).unwrap().len(), 1);
    assert_eq!(
        control::lookup_alias_all_types(&conn, "sqlite")
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        control::lookup_alias_all_types(&conn, "d. richard hipp")
            .unwrap()
            .len(),
        1
    );

    // The knowledge plane: the MENTIONS edge and the fact edge (§8 Stage 4).
    let mut entity_ids = Vec::new();
    for entity_type in [
        "PERSON",
        "PRODUCT",
        "CONCEPT",
        "EVENT",
        "LOCATION",
        "ORGANIZATION",
    ] {
        entity_ids.extend(
            control::canonical_names(&conn, entity_type)
                .unwrap()
                .into_iter()
                .map(|candidate| candidate.entity_id),
        );
    }
    let mentions = knowledge
        .chunks_for_entities(&entity_ids.iter().map(String::as_str).collect::<Vec<_>>())
        .await
        .unwrap();
    assert_eq!(mentions.len(), 1, "the chunk mentions both entities");
    let facts = knowledge
        .facts_within_hops(
            &entity_ids.iter().map(String::as_str).collect::<Vec<_>>(),
            1,
        )
        .await
        .unwrap();
    assert_eq!(facts.len(), 1, "one fact edge: {facts:?}");
    assert_eq!(facts[0].predicate, ohara::knowledge::Predicate::CreatedBy);
    assert_eq!(facts[0].support_count, 1);

    // §6: EXTRACT has no successor — the chain ends here, fully audited.
    assert!(
        control::claim_next(&conn, ohara::control::Stage::Extract, "probe", NOW, 60)
            .unwrap()
            .is_none(),
        "completed EXTRACT is not claimable again"
    );
}

#[tokio::test]
async fn indexed_graph_answers_entity_queries_via_the_graph_path() {
    // Same scripted extractor as the NEW→INDEXED test: one CREATED_BY triplet
    // with SQLite as the subject.
    struct ScriptedLlm;
    #[async_trait::async_trait]
    impl ohara::llm::Llm for ScriptedLlm {
        fn provider_name(&self) -> &'static str {
            "scripted"
        }

        async fn complete(
            &self,
            _req: ohara::llm::CompletionRequest,
        ) -> Result<ohara::llm::CompletionResponse, ohara::llm::LlmError> {
            Ok(ohara::llm::CompletionResponse {
                text: r#"{"triplets":[{"subject":"SQLite","subject_type":"PRODUCT",
                     "predicate":"CREATED_BY","object":"D. Richard Hipp",
                     "object_type":"PERSON"}]}"#
                    .to_string(),
                prompt_tokens: 40,
                completion_tokens: 30,
            })
        }
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    let config = fixture(dir.path(), "");
    let conn = control::connect(config.db_path()).unwrap();
    let doc_id = enqueue_url(&conn, &dir.path().join("data"), "https://example.com/a");
    let knowledge = memory_knowledge();

    let worker = Arc::new(
        Worker::with_ports(
            Arc::clone(&config),
            Arc::new(FakeFetcher {
                result: Fake::Html(ARTICLE_HTML),
            }),
            Arc::new(ohara::pipeline::ReadabilityExtractor),
            Arc::new(FixedEmbedder),
            Arc::clone(&knowledge),
            Arc::new(ScriptedLlm),
        )
        .unwrap(),
    );
    for _ in 0..4 {
        tick(&worker).await;
    }
    let doc = control::get(&conn, &doc_id).unwrap().unwrap();
    assert_eq!(doc.status, DocStatus::Indexed);

    // The Stage 5 read side over the Stage 4 write side (§15 step 6): a query
    // naming the entity resolves it via the typed alias, and the graph path
    // surfaces the mentioning chunk.
    let normalizer =
        ohara::pipeline::WhatlangNormalizer::new(config.retrieval().detection_confidence_floor());
    let retriever = ohara::pipeline::Retriever::new(
        &conn,
        knowledge.as_ref(),
        &FixedEmbedder,
        &normalizer,
        &ohara::pipeline::IdentityReranker,
        ohara::knowledge::ModelId::new(config.knowledge().read_model()),
        config.retrieval().clone(),
    );

    // Query entities: the "sqlite" alias hit is exact and first.
    let entities = retriever.query_entities("sqlite created by").await.unwrap();
    assert!(
        entities
            .iter()
            .any(|e| !e.entity_id.is_empty()
                && e.source == ohara::pipeline::QueryEntitySource::Alias),
        "the typed alias must resolve: {entities:?}"
    );
    let sqlite_id: &str = entities
        .iter()
        .find(|e| e.source == ohara::pipeline::QueryEntitySource::Alias)
        .map(|e| e.entity_id.as_str())
        .unwrap();

    // The full query fuses all three paths; the chunk that mentions SQLite
    // must rank. The fake embedder's vectors are coarse, so assert presence in
    // the pool rather than the top slot.
    let hits = retriever.query("sqlite", 5).await.unwrap();
    let chunk_ids = knowledge.chunks_for_entities(&[sqlite_id]).await.unwrap();
    assert_eq!(chunk_ids.len(), 1, "one mentioning chunk");
    assert!(
        hits.iter().any(|h| h.chunk_id == chunk_ids[0]),
        "the mentioning chunk must be a retrieval candidate, got {hits:?}"
    );
}

/// Live end-to-end probe (§14 + §15 step 4 acceptance): the real `Worker::new`
/// stack — real HTTP fetch, readability clean, chunker, the pinned ONNX
/// embedder, and the configured knowledge services — drives one document to
/// `VECTORIZED`. Ignores by default: needs the network (Wikipedia) and the
/// pinned model, cached once under `data/models` (we symlink the shared cache
/// to avoid re-downloading 130 MB per run). Run with `cargo test --ignored`.
#[tokio::test]
#[ignore = "live: fetches a real article and embeds with the real pinned model"]
async fn live_worker_drives_a_document_to_vectorized() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    // Reuse the machine-wide model cache so the run is offline after the very
    // first fetch of the pinned model.
    if let Some(home) = std::env::var_os("HOME") {
        let shared = std::path::PathBuf::from(home).join(".cache/ohara-test/models");
        if shared.is_dir() {
            std::os::unix::fs::symlink(shared, data.join("models")).unwrap();
        }
    }
    let toml_path = dir.path().join("ohara.toml");
    std::fs::write(
        &toml_path,
        format!(
            "data_dir = {:?}\n[pipeline]\ngraph_enabled = false\n",
            data.display()
        ),
    )
    .unwrap();
    let config = Arc::new(Config::load(Some(&toml_path)).unwrap());
    let conn = control::connect(config.db_path()).unwrap();
    let doc_id = enqueue_url(&conn, &data, "https://en.wikipedia.org/wiki/SQLite");
    drop(conn);

    // REAL ports: engine fetcher + readability + ONNX embedder + remote stores.
    let worker = Arc::new(Worker::new(Arc::clone(&config)).await.unwrap());
    for _ in 0..3 {
        tick(&worker).await;
    }

    let conn = control::connect(config.db_path()).unwrap();
    let doc = control::get(&conn, &doc_id).unwrap().unwrap();
    assert_eq!(
        doc.status,
        DocStatus::Vectorized,
        "live run reached VECTORIZE"
    );
    assert!(doc.chunk_count >= 1);
    // FTS sees the live-cleaned text (§5 trigger sync).
    assert!(
        !control::search_bm25(&conn, "database", 5)
            .unwrap()
            .is_empty()
    );
}
