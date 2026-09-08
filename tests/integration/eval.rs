//! Golden-set evaluation (§14, §15 step 5): the retrieval baseline measured on
//! a synthetic corpus where relevance is true by construction.
//!
//! Corpus: 12 topics × 4 sections = 48 chunks; every chunk is identified by a
//! unique (topic, section-key) pair baked into its text. 50 queries — one per
//! chunk plus two paraphrases — each with exactly one relevant chunk. Each
//! topic also seeds a CONCEPT entity (typed alias + `EntityNames` vector) with
//! `:MENTIONS` edges from its chunks, so the graph path has real structure.
//!
//! Metrics (§8 Stage 5.7 / §14): recall@20 per path (BM25, vector, graph), MRR
//! of the fused list, rerank delta (reranked top-5 vs fused top-5).
//!
//! Two runs: `eval_retrieval_baseline_machinery` is hermetic (fakes) and proves
//! the machinery; `eval_retrieval_baseline_real_models` (ignored — fetches the
//! pinned models on first use) measures the real embedder + reranker and is the
//! run to repeat before/after any retrieval change.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // §10: tests unwrap freely

use std::collections::HashSet;

use ohara::config::Config;
use ohara::control::{self, NewChunkRow, NewDocument};
use ohara::knowledge::{ChunkFilter, KnowledgeStore, ModelId, VectorSpace};
use ohara::pipeline::{Embedder, IdentityReranker, Retriever, ScoredChunk, WhatlangNormalizer};

const NOW: &str = "2026-09-06 12:00:00";

/// Distinct topics — each becomes one document with four section chunks.
const TOPICS: [&str; 12] = [
    "wal checkpointing",
    "lease claiming",
    "reciprocal rank fusion",
    "stemming pipelines",
    "cosine similarity",
    "gzip payloads",
    "robots parsing",
    "vector quantization",
    "merkle proofs",
    "consensus quorum",
    "bloom filters",
    "scheduler budgets",
];

/// Section keys — orthogonal to topics so `(topic, key)` identifies a chunk.
const SECTION_KEYS: [&str; 4] = ["throttling", "tombstones", "backpressure", "compaction"];

/// One golden query: text + the single relevant chunk id.
struct GoldenQuery {
    text: String,
    relevant: String,
}

struct EvalFixture {
    #[allow(dead_code)] // holds the tempdir open for the test's duration
    dir: tempfile::TempDir,
    store: std::path::PathBuf,
    #[allow(dead_code)] // holds the tempdir open for the test
    data_dir: std::path::PathBuf,
    /// Validated from the fixture's TOML — feeds the retriever's knobs.
    config: Config,
}

/// Seeds the graph side for one topic (§8 Stage 4 write shape): a CONCEPT
/// entity with a typed alias, its `EntityNames` vector, `:MENTIONS` edges from
/// every chunk, so the graph path has structure to traverse.
async fn seed_topic_graph(
    conn: &rusqlite::Connection,
    knowledge: &ohara::knowledge::LadybugStore,
    embedder: &dyn Embedder,
    t: usize,
    topic: &str,
    chunk_ids: &[String],
) {
    let topic_entity = format!("ent-{t}");
    let normalized = ohara::text::normalize_surface_form(topic);
    control::ensure_entity(conn, &topic_entity, &normalized, "CONCEPT", None).unwrap();
    control::upsert_alias(conn, &normalized, "CONCEPT", &topic_entity).unwrap();
    knowledge
        .upsert_entity(&ohara::knowledge::EntityRecord {
            entity_id: topic_entity.clone(),
            canonical_name: topic.to_string(),
            entity_type: ohara::knowledge::EntityType::Concept,
            subtype: None,
        })
        .await
        .unwrap();
    let name_vec = embedder.embed(&[topic]).unwrap().remove(0);
    knowledge
        .upsert_vectors(VectorSpace::EntityNames, "", &[&topic_entity], &[name_vec])
        .await
        .unwrap();
    for chunk_id in chunk_ids {
        knowledge
            .link_mention(chunk_id, &topic_entity)
            .await
            .unwrap();
    }
}

/// Builds the corpus: documents + chunk rows in the registry, vectors in the
/// knowledge store, keyed by the embedder's own model id.
async fn build_corpus(
    embedder: &dyn Embedder,
) -> (
    EvalFixture,
    ohara::knowledge::LadybugStore,
    Vec<GoldenQuery>,
) {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let toml_path = dir.path().join("ohara.toml");
    std::fs::write(&toml_path, format!("data_dir = {:?}\n", data_dir.display())).unwrap();
    let config = Config::load(Some(&toml_path)).unwrap();
    let store = dir.path().join("store.db");
    let conn = control::connect(&store).unwrap();

    let space = VectorSpace::Chunks {
        model_id: ModelId::new(embedder.model_id().to_string()),
    };
    let knowledge = ohara::knowledge::LadybugStore::in_memory(embedder.dim()).unwrap();

    let mut queries: Vec<GoldenQuery> = Vec::new();
    for (t, topic) in TOPICS.iter().enumerate() {
        let doc_id = control::insert_new(
            &conn,
            &data_dir,
            &NewDocument {
                source_url: format!("https://eval.example/{t}"),
                source_url_normalized: format!("https://eval.example/{t}"),
                priority: 5,
                pipeline_version: "0.1.0".to_string(),
            },
            NOW,
        )
        .unwrap()
        .doc_id()
        .to_string();
        let rows: Vec<NewChunkRow> = SECTION_KEYS
            .iter()
            .enumerate()
            .map(|(s, key)| {
                let text = format!(
                    "On {topic}: the {key} aspect in detail. {topic} {key} mechanics, \
                     operational guidance, and the edge cases practitioners hit when \
                     the {key} path interacts with the rest of the system."
                );
                NewChunkRow {
                    chunk_id: ohara::text::sha256_hex(&format!("{doc_id}:{s}")),
                    seq: i64::try_from(s).unwrap_or(i64::MAX),
                    header_path: topic.to_string(),
                    text: text.clone(),
                    embed_text: text,
                    token_count: 40,
                    embedding_model: embedder.model_id().to_string(),
                    content_hash: ohara::text::sha256_hex(&format!("{topic}/{key}")),
                }
            })
            .collect();
        for (s, key) in SECTION_KEYS.iter().enumerate() {
            queries.push(GoldenQuery {
                text: format!("how does {key} work in {topic}"),
                relevant: rows[s].chunk_id.clone(),
            });
        }
        control::replace_chunks(&conn, &doc_id, &rows).unwrap();
        for row in &rows {
            let vec = embedder.embed(&[&row.embed_text]).unwrap().remove(0);
            knowledge
                .upsert_vectors(space.clone(), &doc_id, &[row.chunk_id.as_str()], &[vec])
                .await
                .unwrap();
        }

        // The graph side (§8 Stage 4 write shape) — every chunk of this topic
        // mentions the topic entity.
        seed_topic_graph(
            &conn,
            &knowledge,
            embedder,
            t,
            topic,
            &rows.iter().map(|r| r.chunk_id.clone()).collect::<Vec<_>>(),
        )
        .await;
    }
    // Two paraphrase queries beyond the 48 canonical ones (§14: 50 total).
    queries.push(GoldenQuery {
        text: "throttling in reciprocal rank fusion".to_string(),
        relevant: ohara::text::sha256_hex(&format!(
            "{}:0",
            find_doc(&conn, "https://eval.example/2")
        )),
    });
    queries.push(GoldenQuery {
        text: "bloom filters compaction details".to_string(),
        relevant: ohara::text::sha256_hex(&format!(
            "{}:3",
            find_doc(&conn, "https://eval.example/10")
        )),
    });
    drop(conn);
    let fixture = EvalFixture {
        dir,
        store,
        data_dir,
        config,
    };
    (fixture, knowledge, queries)
}

fn find_doc(conn: &rusqlite::Connection, url: &str) -> String {
    conn.query_row(
        "SELECT doc_id FROM documents WHERE source_url_normalized = ?1",
        [url],
        |r| r.get(0),
    )
    .unwrap()
}

/// The fixture's validated retrieval knobs (§8 Stage 5.2).
fn fixture_config(fixture: &EvalFixture) -> ohara::config::RetrievalConfig {
    fixture.config.retrieval().clone()
}

/// Per-path metrics: recall@20 (path's own top-20) and the fused/reranked MRR.
#[derive(Debug)]
struct Metrics {
    bm25_recall20: f64,
    vector_recall20: f64,
    graph_recall20: f64,
    fused_mrr: f64,
    fused_top5_mrr: f64,
    reranked_top5_mrr: f64,
}

impl Metrics {
    fn rerank_delta(&self) -> f64 {
        self.reranked_top5_mrr - self.fused_top5_mrr
    }

    fn report(name: &str) -> impl Fn(&Metrics) + '_ {
        move |m: &Metrics| {
            println!(
                "[{name}] bm25_recall20={:.3} vector_recall20={:.3} graph_recall20={:.3} \
                 fused_mrr={:.3} fused_top5_mrr={:.3} reranked_top5_mrr={:.3} \
                 rerank_delta={:+.3}",
                m.bm25_recall20,
                m.vector_recall20,
                m.graph_recall20,
                m.fused_mrr,
                m.fused_top5_mrr,
                m.reranked_top5_mrr,
                m.rerank_delta()
            );
        }
    }
}

/// Runs every golden query through the retriever and the raw paths.
async fn run_eval(
    fixture: &EvalFixture,
    knowledge: &ohara::knowledge::LadybugStore,
    embedder: &dyn Embedder,
    queries: &[GoldenQuery],
    reranker: &dyn ohara::pipeline::Reranker,
) -> Metrics {
    let conn = control::connect(&fixture.store).unwrap();
    let space = VectorSpace::Chunks {
        model_id: ModelId::new(embedder.model_id().to_string()),
    };

    let retriever = Retriever::new(
        &conn,
        knowledge,
        embedder,
        &WhatlangNormalizer,
        reranker,
        fixture_config(fixture),
    );
    let mut bm25_hits = 0usize;
    let mut vec_hits = 0usize;
    let mut graph_hits = 0usize;
    let mut fused_reciprocal = 0.0f64;
    let mut fused_top5_reciprocal = 0.0f64;
    let mut reranked5_reciprocal = 0.0f64;

    for q in queries {
        // Raw-path rankings, straight from the machinery (§14: recall@20 per
        // path), computed via the same primitives the retriever uses.
        let bm25_expr = ohara::pipeline::fts_match_expression(&q.text);
        let mut stmt = conn
            .prepare(
                "SELECT c.chunk_id FROM chunks_fts f JOIN chunks c ON c.id = f.rowid
                  WHERE chunks_fts MATCH ?1 ORDER BY bm25(chunks_fts) LIMIT 20",
            )
            .unwrap();
        let bm25_top: HashSet<String> = stmt
            .query_map([&bm25_expr], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        drop(stmt);
        if bm25_top.contains(&q.relevant) {
            bm25_hits += 1;
        }

        let qv = embedder.embed(&[&q.text]).unwrap().remove(0);
        let knn = knowledge
            .knn(space.clone(), &qv, 20, &ChunkFilter {})
            .await
            .unwrap();
        if knn.iter().any(|h| h.id == q.relevant) {
            vec_hits += 1;
        }

        // Graph path raw recall (§14): the query's entities -> their
        // mentioning chunks. Mirrors the retriever's phrase-window alias pass.
        let normalized = ohara::text::normalize_surface_form(&q.text);
        let terms: Vec<&str> = normalized.split_whitespace().collect();
        let mut entity_ids: Vec<String> = Vec::new();
        for window in (1..=terms.len()).rev() {
            for start in 0..=terms.len() - window {
                entity_ids.extend(
                    control::lookup_alias_all_types(&conn, &terms[start..start + window].join(" "))
                        .unwrap(),
                );
            }
        }
        entity_ids.sort();
        entity_ids.dedup();
        if !entity_ids.is_empty() {
            let ids: Vec<&str> = entity_ids.iter().map(String::as_str).collect();
            let mentioned = knowledge.chunks_for_entities(&ids).await.unwrap();
            if mentioned.contains(&q.relevant) {
                graph_hits += 1;
            }
        }

        // Fused list via the retriever (identity reranker returns fusion order).
        let fused: Vec<ScoredChunk> = retriever
            .query(&q.text, ohara::pipeline::RETRIEVAL_POOL)
            .await
            .unwrap();
        let rank = fused.iter().position(|h| h.chunk_id == q.relevant);
        if let Some(r) = rank {
            fused_reciprocal += 1.0 / (f64::from(u32::try_from(r).unwrap_or(u32::MAX)) + 1.0);
        }
        if rank.is_some_and(|r| r < 5) {
            fused_top5_reciprocal +=
                1.0 / (f64::from(u32::try_from(rank.unwrap()).unwrap_or(u32::MAX)) + 1.0);
        }

        // Reranked top-5 via the injected reranker.
        let reranked_list = reranker
            .rerank(&q.text, fused.clone())
            .await
            .unwrap_or_else(|_| fused.clone());
        if let Some(r) = reranked_list.iter().position(|h| h.chunk_id == q.relevant)
            && r < 5
        {
            reranked5_reciprocal += 1.0 / (f64::from(u32::try_from(r).unwrap_or(u32::MAX)) + 1.0);
        }
    }

    #[allow(clippy::cast_precision_loss)] // counts ≤ 50; precision loss impossible
    let n = queries.len() as f64;
    Metrics {
        bm25_recall20: f64::from(u32::try_from(bm25_hits).unwrap_or(u32::MAX)) / n,
        vector_recall20: f64::from(u32::try_from(vec_hits).unwrap_or(u32::MAX)) / n,
        graph_recall20: f64::from(u32::try_from(graph_hits).unwrap_or(u32::MAX)) / n,
        fused_mrr: fused_reciprocal / n,
        fused_top5_mrr: fused_top5_reciprocal / n,
        reranked_top5_mrr: reranked5_reciprocal / n,
    }
}

/// The machinery run (hermetic): fake embedder + identity reranker. The floors
/// prove the paths, fusion, and metric plumbing; the real-model run below is
/// the one that gates retrieval changes.
#[tokio::test]
async fn eval_retrieval_baseline_machinery() {
    struct FakeEmbedder;

    impl Embedder for FakeEmbedder {
        fn model_id(&self) -> &'static str {
            "eval-fake-embedder"
        }

        fn dim(&self) -> usize {
            4
        }

        fn count_tokens(&self, text: &str) -> usize {
            text.split_whitespace().count() + 2
        }

        fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, ohara::pipeline::EmbedError> {
            // Word-hash bag (same shape as the unit-test fake): shared
            // vocabulary → shared vectors, so the lexical golden set works.
            Ok(texts
                .iter()
                .map(|t| {
                    let mut v = [0.0f32; 4];
                    for word in t.split_whitespace() {
                        let b = usize::from(word.as_bytes().first().copied().unwrap_or(b' '));
                        v[b % 4] += 1.0;
                    }
                    let norm = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2] + v[3] * v[3]).sqrt();
                    if norm > 0.0 {
                        v.map(|x| x / norm).to_vec()
                    } else {
                        v.to_vec()
                    }
                })
                .collect())
        }
    }

    let embedder = FakeEmbedder;
    let (fixture, knowledge, queries) = build_corpus(&embedder).await;
    assert_eq!(queries.len(), 50, "§14: 50-query golden set");

    let metrics = run_eval(&fixture, &knowledge, &embedder, &queries, &IdentityReranker).await;
    Metrics::report("machinery")(&metrics);
    assert!(
        metrics.bm25_recall20 >= 0.95,
        "BM25 machinery floor: {metrics:?}"
    );
    assert!(
        metrics.vector_recall20 >= 0.9,
        "vector machinery floor: {metrics:?}"
    );
    // The graph path resolves the topic entity per query and returns all its
    // mentioning chunks — the relevant one is always among them (§14).
    assert!(
        metrics.graph_recall20 >= 0.9,
        "graph machinery floor: {metrics:?}"
    );
    assert!(metrics.fused_mrr >= 0.7, "fusion floor: {metrics:?}");
}

/// The real-model run (§15 step 5's "measured on the golden set"): the pinned
/// embedder + the int8 bge reranker. Repeat before/after any retrieval change
/// (§14) — it gates HNSW knobs later (§11).
#[tokio::test]
#[cfg(feature = "onnx-embedder")]
#[ignore = "loads the pinned embedder + reranker models (cached under ~/.cache/ohara-test after first fetch)"]
async fn eval_retrieval_baseline_real_models() {
    let dir = tempfile::tempdir().unwrap();
    let models = dir.path().join("models");
    match std::env::var_os("HOME") {
        Some(home) => {
            // Reuse the machine-wide cache when present (symlink, so the first
            // real fetch persists across runs); fall back to a fresh dir.
            let shared = std::path::PathBuf::from(home).join(".cache/ohara-test/models");
            if shared.is_dir() {
                std::os::unix::fs::symlink(shared, &models).unwrap();
            } else {
                std::fs::create_dir_all(&models).unwrap();
            }
        }
        None => {
            std::fs::create_dir_all(&models).unwrap();
        }
    }
    let embedder = ohara::pipeline::LocalEmbedder::new(&models).unwrap();
    let reranker = ohara::pipeline::LocalReranker::new(&models).unwrap();

    let (fixture, knowledge, queries) = build_corpus(&embedder).await;
    let metrics = run_eval(&fixture, &knowledge, &embedder, &queries, &reranker).await;
    Metrics::report("real")(&metrics);
    // Floors from the baseline measurement — loosened deliberately; the report
    // line is the working number, these only catch catastrophic regressions.
    assert!(metrics.bm25_recall20 >= 0.9, "BM25 floor: {metrics:?}");
    assert!(metrics.vector_recall20 >= 0.6, "vector floor: {metrics:?}");
    assert!(metrics.fused_mrr >= 0.5, "fusion floor: {metrics:?}");
    assert!(
        metrics.rerank_delta() >= -0.05,
        "rerank must not meaningfully hurt the fused order: {metrics:?}"
    );
}
