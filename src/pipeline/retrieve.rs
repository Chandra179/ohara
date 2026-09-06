//! Stage 5 — Retrieval (§8), baseline form (§15 step 5: BM25 + vector + rerank,
//! no graph path yet): query normalization → two-path candidate generation →
//! Reciprocal Rank Fusion → rerank with degradation. Its two ports,
//! [`QueryNormalizer`] and [`Reranker`] (§1.3), plus the baseline
//! implementations.
//!
//! §8 Stage 5 items that land with later steps: the graph path and query
//! entities (§15 step 6), symspell domain-dictionary correction and optional
//! `HyDE` (ops/eval expansion), synthesis via the `Llm` port.

use async_trait::async_trait;

use crate::Class;
use crate::control::{self, DbError};
use crate::knowledge::{KnowledgeError, KnowledgeStore, ModelId, VectorSpace};
use crate::pipeline::Embedder;

/// Candidate-pool size per path and after fusion (§8 Stage 5: fusion produces
/// the top-50, rerank returns the top-5).
pub const RETRIEVAL_POOL: usize = 50;

/// RRF constant (§8 Stage 5): reciprocal rank fusion with k=60.
const RRF_K: f32 = 60.0;

/// Detected language (§8): the embedder is English-first, so the Stage 2 gate and
/// the query normalizer share this vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Lang {
    /// English — the default target (§4, §8 Stage 2).
    #[default]
    En,
    /// Anything else, until a multilingual model swap (§4).
    Other,
}

/// A chunk candidate with a relevance score — the reranker pool's currency (§8
/// Stage 5: fusion produces the top-50, rerank returns the top-5).
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredChunk {
    /// The chunk's cross-store identity (`sha256(doc_id:seq)`, §3).
    pub chunk_id: String,
    /// Display text.
    pub text: String,
    /// Current relevance score (fused rank or reranker score).
    pub score: f32,
}

/// The query-normalization port (§9): pure; `normalize(q) -> (lang, q')`.
pub trait QueryNormalizer: Send + Sync {
    /// Detects the language and normalizes the query (domain-dictionary
    /// correction, §8 Stage 5). Pure — no I/O; unknown inputs pass through
    /// unchanged with [`Lang::Other`].
    fn normalize(&self, query: &str) -> (Lang, String);
}

/// The reranker port (§9): returns all candidates sorted by relevance, descending.
/// Fallible by contract — callers degrade to fusion order on `Err`, never failing
/// the query (§8 Stage 5).
#[async_trait]
pub trait Reranker: Send + Sync {
    /// Impl name, for metrics and audit.
    fn name(&self) -> &str;

    /// Reranks `candidates` against `query`.
    ///
    /// # Errors
    /// [`RerankError`] — degradation, not propagation (§8 Stage 5).
    async fn rerank(
        &self,
        query: &str,
        candidates: Vec<ScoredChunk>,
    ) -> Result<Vec<ScoredChunk>, RerankError>;
}

/// Reranking failures (§9).
#[derive(Debug, thiserror::Error)]
pub enum RerankError {
    /// The reranker model is not loaded / not reachable.
    #[error("reranker model unavailable: {0}")]
    ModelUnavailable(String),
    /// Inference failed for this query.
    #[error("reranker inference failed: {0}")]
    Inference(String),
}

impl RerankError {
    /// Retry class (§10) — informational: the Stage 5 contract degrades to fusion
    /// order and counts the failure instead of propagating (§8 Stage 5).
    #[must_use]
    pub fn class(&self) -> Class {
        Class::Retry
    }
}

/// Query-path failures (§8 Stage 5): only the rerank step degrades; the
/// retrieval paths propagate — an interactive query must not silently lose a
/// whole path.
#[derive(Debug, thiserror::Error)]
pub enum RetrieveError {
    /// The control store failed.
    #[error("control store: {0}")]
    Control(#[from] DbError),
    /// A raw `SQLite` statement failed (FTS path).
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// The knowledge plane failed (vector path).
    #[error("knowledge store: {0}")]
    Knowledge(#[from] KnowledgeError),
    /// The query could not be embedded.
    #[error("query embedding failed: {0}")]
    Embed(#[from] crate::pipeline::EmbedError),
}

/// The baseline [`QueryNormalizer`] (§8 Stage 5.1): whatlang language
/// detection, trim. Domain-dictionary typo correction (`symspell`) and `HyDE` land
/// with later steps — the port keeps the door open.
///
/// Detection is confidence-gated: whatlang reliably separates languages on
/// sentence-length input (confidence ≈ 1.0) but mislabels short technical
/// queries at 0.02–0.10 (measured: "wal throttling" → Tagalog). Below the
/// 0.5 threshold the result is treated as English — the Stage 2 gate already
/// bounds this tool's corpora to `target_languages`, and [`Lang`] here is
/// metadata for callers, never a hard gate on the query path.
pub struct WhatlangNormalizer;

/// Detection-confidence floor (see type doc): whatlang's short-query noise
/// tops out far below this; real sentence detections land at ≈ 1.0.
const DETECTION_CONFIDENCE_FLOOR: f64 = 0.5;

impl QueryNormalizer for WhatlangNormalizer {
    fn normalize(&self, query: &str) -> (Lang, String) {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return (Lang::Other, String::new());
        }
        let lang = match whatlang::detect(trimmed) {
            Some(info) if info.confidence() >= DETECTION_CONFIDENCE_FLOOR => {
                if info.lang() == whatlang::Lang::Eng {
                    Lang::En
                } else {
                    Lang::Other
                }
            }
            _ => Lang::En,
        };
        (lang, trimmed.to_string())
    }
}

/// FTS5 MATCH expression for a query (§8 Stage 5.3): word tokens only, each
/// double-quoted (operators, quotes, and punctuation in raw user text would be
/// FTS5 syntax), joined with `OR` so any term matches — BM25 ranks the rest.
#[must_use]
pub fn fts_match_expression(query: &str) -> String {
    let tokens: Vec<String> = query
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();
    tokens.join(" OR ")
}

/// The §9 baseline reranker: preserves the fusion order as-is (candidates arrive
/// sorted). Benchmarking baseline and degradation reference (§8 Stage 5.5).
pub struct IdentityReranker;

#[async_trait]
impl Reranker for IdentityReranker {
    fn name(&self) -> &'static str {
        "identity"
    }

    async fn rerank(
        &self,
        _query: &str,
        mut candidates: Vec<ScoredChunk>,
    ) -> Result<Vec<ScoredChunk>, RerankError> {
        candidates.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.chunk_id.cmp(&b.chunk_id))
        });
        Ok(candidates)
    }
}

/// The local cross-encoder reranker (§2, §11.1): `bge-reranker-base` **int8**
/// (~280 MB ONNX export — never fp32), loaded via fastembed's user-defined path
/// from files fetched once into `cache_dir`. Behind the `onnx-embedder` feature.
#[cfg(feature = "onnx-embedder")]
pub struct LocalReranker {
    model: std::sync::Mutex<fastembed::TextRerank>,
}

#[cfg(feature = "onnx-embedder")]
impl LocalReranker {
    /// The pinned reranker (§11.1): int8 export of `bge-reranker-base`.
    const HF_REPO: &str = "Xenova/bge-reranker-base";
    const MODEL_FILE: &str = "onnx/model_quantized.onnx";

    /// Loads (downloading on first use) the pinned int8 reranker.
    ///
    /// # Errors
    /// [`RerankError::ModelUnavailable`] when files cannot be fetched or the
    /// runtime cannot load the graph — construction-time failure, not query-time.
    pub fn new(cache_dir: &std::path::Path) -> Result<Self, RerankError> {
        let unavailable = |e: String| RerankError::ModelUnavailable(e);
        let api = hf_hub::api::sync::ApiBuilder::new()
            .with_cache_dir(cache_dir.to_path_buf())
            .build()
            .map_err(|e| unavailable(format!("HF hub API init failed: {e}")))?;
        let repo = api.model(Self::HF_REPO.to_string());
        let read = |name: &str| -> Result<Vec<u8>, RerankError> {
            std::fs::read(
                repo.get(name)
                    .map_err(|e| unavailable(format!("{name}: {e}")))?,
            )
            .map_err(|e| unavailable(format!("{name}: {e}")))
        };
        let model_path = repo
            .get(Self::MODEL_FILE)
            .map_err(|e| unavailable(format!("{}: {e}", Self::MODEL_FILE)))?;
        let model = fastembed::TextRerank::try_new_from_user_defined(
            fastembed::UserDefinedRerankingModel::new(
                model_path,
                fastembed::TokenizerFiles {
                    tokenizer_file: read("tokenizer.json")?,
                    config_file: read("config.json")?,
                    special_tokens_map_file: read("special_tokens_map.json")?,
                    tokenizer_config_file: read("tokenizer_config.json")?,
                },
            ),
            fastembed::RerankInitOptionsUserDefined::new(),
        )
        .map_err(|e| unavailable(format!("pinned reranker failed to load: {e}")))?;
        Ok(Self {
            model: std::sync::Mutex::new(model),
        })
    }
}

#[cfg(feature = "onnx-embedder")]
#[async_trait]
impl Reranker for LocalReranker {
    fn name(&self) -> &'static str {
        "bge-reranker-base:int8"
    }

    async fn rerank(
        &self,
        query: &str,
        candidates: Vec<ScoredChunk>,
    ) -> Result<Vec<ScoredChunk>, RerankError> {
        if candidates.is_empty() {
            return Ok(candidates);
        }
        let mut model = self
            .model
            .lock()
            .map_err(|_| RerankError::ModelUnavailable("reranker mutex poisoned".into()))?;
        let texts: Vec<&str> = candidates.iter().map(|c| c.text.as_str()).collect();
        // §11.1: query-time only; 50 × 400-token pairs ≈ 100–500 ms on the
        // reference CPU.
        let results = model
            .rerank(query, texts, false, None)
            .map_err(|e| RerankError::Inference(e.to_string()))?;
        let mut reranked: Vec<ScoredChunk> = results
            .into_iter()
            .map(|r| ScoredChunk {
                score: r.score,
                ..candidates[r.index].clone()
            })
            .collect();
        reranked.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.chunk_id.cmp(&b.chunk_id))
        });
        Ok(reranked)
    }
}

/// The two-path fusion pool: each list contributes `1/(k + rank)` (§8).
fn rrf_fuse(
    lists: [Vec<(String, String)>; 2], // (chunk_id, text) per path, best first
) -> Vec<ScoredChunk> {
    let mut scores: std::collections::HashMap<String, (f32, String)> =
        std::collections::HashMap::new();
    for list in lists {
        for (rank, (chunk_id, text)) in list.into_iter().enumerate() {
            let entry = scores
                .entry(chunk_id)
                .or_insert_with(|| (0.0, text.clone()));
            entry.0 += 1.0 / (RRF_K + f32::from(u16::try_from(rank).unwrap_or(u16::MAX)) + 1.0);
        }
    }
    let mut fused: Vec<ScoredChunk> = scores
        .into_iter()
        .map(|(chunk_id, (score, text))| ScoredChunk {
            chunk_id,
            text,
            score,
        })
        .collect();
    fused.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.chunk_id.cmp(&b.chunk_id))
    });
    fused
}

/// The retrieval engine (§8 Stage 5): two candidate paths over the stores,
/// fusion, rerank. Ports are injected (§9); the connection borrows the caller's
/// control store. `query` is async — its only blocking work is one query
/// embedding (ms-scale, unlike the worker's batch path, §9) and the `SQLite`
/// BM25 scan.
pub struct Retriever<'a> {
    conn: &'a rusqlite::Connection,
    knowledge: &'a dyn KnowledgeStore,
    embedder: &'a dyn Embedder,
    normalizer: &'a dyn QueryNormalizer,
    reranker: &'a dyn Reranker,
}

impl<'a> Retriever<'a> {
    /// Builds a retriever; must be called inside a tokio runtime (the vector
    /// path drives the async [`KnowledgeStore`] port from this sync context
    /// via the captured handle).
    #[must_use]
    pub fn new(
        conn: &'a rusqlite::Connection,
        knowledge: &'a dyn KnowledgeStore,
        embedder: &'a dyn Embedder,
        normalizer: &'a dyn QueryNormalizer,
        reranker: &'a dyn Reranker,
    ) -> Self {
        Self {
            conn,
            knowledge,
            embedder,
            normalizer,
            reranker,
        }
    }

    /// Runs one query (§8 Stage 5): normalize → BM25 + vector paths → RRF
    /// fusion → rerank with degradation → top `top_k`.
    ///
    /// # Errors
    /// [`RetrieveError`] — the rerank step degrades to fusion order instead
    /// (§8 Stage 5.5); everything else propagates.
    pub async fn query(
        &self,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<ScoredChunk>, RetrieveError> {
        let (_lang, normalized) = self.normalizer.normalize(query);

        // Path 1 — BM25 via the trigger-synced chunks_fts (§8: embeddings are
        // weak on exact identifiers like "SQLite").
        let bm25 = self.bm25_path(&normalized)?;

        // Path 2 — vector KNN over the read-model collection (§4's atomic read
        // switch).
        let vector = self.vector_path(&normalized).await?;

        // Fusion (§8 Stage 5.4): RRF with k=60 → top-50 pool.
        let pool: Vec<ScoredChunk> = rrf_fuse([bm25, vector])
            .into_iter()
            .take(RETRIEVAL_POOL)
            .collect();

        // Rerank with degradation (§8 Stage 5.5): on RerankError the fusion
        // order is returned as-is — the query never fails on the reranker.
        let ranked = match self.reranker.rerank(query, pool.clone()).await {
            Ok(reranked) => reranked,
            Err(_degraded) => pool,
        };
        Ok(ranked.into_iter().take(top_k).collect())
    }

    /// BM25 path (§8 Stage 5.3): `chunks_fts` `MATCH`, bm25 order, best first.
    fn bm25_path(&self, normalized: &str) -> Result<Vec<(String, String)>, RetrieveError> {
        let expression = fts_match_expression(normalized);
        if expression.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT c.chunk_id, c.text
               FROM chunks_fts f JOIN chunks c ON c.id = f.rowid
              WHERE chunks_fts MATCH ?1
              ORDER BY bm25(chunks_fts)
              LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![
                expression,
                i64::try_from(RETRIEVAL_POOL).unwrap_or(i64::MAX)
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Vector path (§8 Stage 5.3): embed the query, KNN the read-model
    /// collection, hydrate texts from the registry.
    async fn vector_path(&self, normalized: &str) -> Result<Vec<(String, String)>, RetrieveError> {
        if normalized.trim().is_empty() {
            return Ok(Vec::new());
        }
        let embedded = self.embedder.embed(&[normalized])?;
        let Some(q) = embedded.into_iter().next() else {
            return Ok(Vec::new());
        };
        let space = VectorSpace::Chunks {
            model_id: ModelId::new(self.embedder.model_id().to_string()),
        };
        let hits = self
            .knowledge
            .knn(space, &q, RETRIEVAL_POOL, &crate::knowledge::ChunkFilter {})
            .await?;
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        let hydrated = control::chunks_by_ids(self.conn, &ids)?;
        let by_id: std::collections::HashMap<String, String> =
            hydrated.into_iter().map(|c| (c.chunk_id, c.text)).collect();
        // KNN order is the ranking; chunks missing from the registry (a
        // deletion race) drop out here.
        Ok(hits
            .into_iter()
            .filter_map(|h| by_id.get(&h.id).map(|text| (h.id, text.clone())))
            .collect())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use crate::config::Config;
    use crate::control::testing::seed_doc;
    use crate::control::{self, NewChunkRow};
    use crate::knowledge::VectorSpace;
    use crate::pipeline::test_support::{FakeEmbedder, InMemoryKnowledge};
    use crate::text::sha256_hex;

    use super::{IdentityReranker, Retriever, WhatlangNormalizer, fts_match_expression};
    use crate::knowledge::KnowledgeStore;
    use crate::pipeline::{Embedder, QueryNormalizer};

    #[test]
    fn fts_expression_quotes_tokens_and_joins_with_or() {
        assert_eq!(
            fts_match_expression("wal checkpointing"),
            "\"wal\" OR \"checkpointing\""
        );
        // FTS5 syntax characters never reach the expression raw.
        assert_eq!(
            fts_match_expression("a AND b OR \"NEAR\""),
            "\"a\" OR \"AND\" OR \"b\" OR \"OR\" OR \"NEAR\""
        );
        assert_eq!(fts_match_expression("!!! ..."), "");
        assert_eq!(
            fts_match_expression("snake_case words"),
            "\"snake_case\" OR \"words\""
        );
    }

    #[test]
    fn normalizer_detects_english_and_passes_through() {
        let n = WhatlangNormalizer;
        // Short technical queries sit below whatlang's reliable-confidence
        // range: the documented fallback is En (the Stage 2 gate bounds the
        // corpus languages anyway).
        let (lang, q) = n.normalize("  how does WAL checkpointing work? ");
        assert_eq!(lang, super::Lang::En);
        assert_eq!(q, "how does WAL checkpointing work?");
        assert_eq!(n.normalize("").0, super::Lang::Other);
        assert_eq!(n.normalize("???").0, super::Lang::En);
        // Confident non-English input is flagged Other (§9 passthrough rule
        // applies to the query text; detection is metadata).
        let (lang, _) = n.normalize(
            "Der schnelle braune Fuchs springt ueber den faulen Hund in diesem Garten voller Blumen",
        );
        assert_eq!(lang, super::Lang::Other);
    }

    /// A failing reranker: every call errors (§8 Stage 5.5 degradation path).
    struct BrokenReranker;

    #[async_trait::async_trait]
    impl super::Reranker for BrokenReranker {
        fn name(&self) -> &'static str {
            "broken"
        }

        async fn rerank(
            &self,
            _query: &str,
            _candidates: Vec<super::ScoredChunk>,
        ) -> Result<Vec<super::ScoredChunk>, super::RerankError> {
            Err(super::RerankError::Inference("boom".to_string()))
        }
    }

    struct Fixture {
        #[allow(dead_code)] // keeps the tempdir alive for the test's duration
        dir: tempfile::TempDir,
        #[allow(dead_code)] // loaded to validate the fixture's TOML once
        config: Arc<Config>,
        store: std::path::PathBuf,
        doc_ids: Vec<String>,
    }

    /// Seeds two documents with three chunks each; chunk texts carry distinct
    /// keywords so BM25 and the fake-embedder vector path both resolve them.
    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("data")).unwrap();
        let toml_path = dir.path().join("ohara.toml");
        std::fs::write(
            &toml_path,
            format!("data_dir = {:?}\n", dir.path().join("data").display()),
        )
        .unwrap();
        let config = Arc::new(Config::load(Some(&toml_path)).unwrap());
        let store = dir.path().join("store.db");
        let conn = control::connect(&store).unwrap();
        let mut doc_ids = Vec::new();
        for slug in ["wal", "hnsw"] {
            let doc_id = seed_doc(&conn, slug);
            doc_ids.push(doc_id.clone());
            let keywords: &[&str] = if slug == "wal" {
                &["checkpointing", "throttling", "truncation"]
            } else {
                &["ef-search", "inserts", "rebuilds"]
            };
            let rows: Vec<NewChunkRow> = keywords
                .iter()
                .enumerate()
                .map(|(seq, kw)| {
                    let text = format!("{slug} keyword {kw} explained with operational depth");
                    NewChunkRow {
                        chunk_id: sha256_hex(&format!("{doc_id}:{seq}")),
                        seq: i64::try_from(seq).unwrap_or(i64::MAX),
                        header_path: slug.to_string(),
                        text: text.clone(),
                        embed_text: text,
                        token_count: 20,
                        embedding_model: "fake-embedder".to_string(),
                        content_hash: sha256_hex(kw),
                    }
                })
                .collect();
            control::replace_chunks(&conn, &doc_id, &rows).unwrap();
        }
        drop(conn);
        Fixture {
            dir,
            config,
            store,
            doc_ids,
        }
    }

    async fn knowledge_with_vectors(fx: &Fixture, embedder: &FakeEmbedder) -> InMemoryKnowledge {
        let knowledge = InMemoryKnowledge::default();
        let conn = control::connect(&fx.store).unwrap();
        let space = VectorSpace::Chunks {
            model_id: crate::knowledge::ModelId::new(embedder.model_id().to_string()),
        };
        for doc_id in &fx.doc_ids {
            for sig in control::chunk_signatures(&conn, doc_id).unwrap() {
                let text: String = conn
                    .query_row(
                        "SELECT embed_text FROM chunks WHERE chunk_id = ?1",
                        [&sig.chunk_id],
                        |r| r.get(0),
                    )
                    .unwrap();
                let vec = embedder.embed(&[&text]).unwrap().remove(0);
                knowledge
                    .upsert_vectors(space.clone(), doc_id, &[&sig.chunk_id], &[vec])
                    .await
                    .unwrap();
            }
        }
        knowledge
    }

    #[tokio::test]
    async fn query_ranks_the_matching_chunk_first() {
        let fx = fixture();
        let conn = control::connect(&fx.store).unwrap();
        let embedder = FakeEmbedder::new();
        let knowledge = knowledge_with_vectors(&fx, &embedder).await;
        let retriever = Retriever::new(
            &conn,
            &knowledge,
            &embedder,
            &WhatlangNormalizer,
            &IdentityReranker,
        );

        let hits = retriever.query("wal throttling", 5).await.unwrap();
        assert!(!hits.is_empty());
        let first = &hits[0];
        assert!(first.text.contains("throttling"), "got {first:?}");
        assert!(first.text.contains("wal"));

        // FTS-only term (no vector support in the fake for this shape): the
        // BM25 path alone must surface it.
        let hits = retriever.query("hnsw rebuilds", 5).await.unwrap();
        assert!(hits[0].text.contains("rebuilds"), "got {:?}", hits[0].text);
    }

    #[tokio::test]
    async fn top_k_truncates_and_rerank_degradation_preserves_fusion_order() {
        let fx = fixture();
        let conn = control::connect(&fx.store).unwrap();
        let embedder = FakeEmbedder::new();
        let knowledge = knowledge_with_vectors(&fx, &embedder).await;
        let retriever = Retriever::new(
            &conn,
            &knowledge,
            &embedder,
            &WhatlangNormalizer,
            &BrokenReranker,
        );

        // Broken reranker: degradation, not failure (§8 Stage 5.5) — the
        // fusion order comes back, truncated to top_k.
        let hits = retriever.query("wal checkpointing", 2).await.unwrap();
        assert!(hits.len() <= 2);
        assert!(hits[0].text.contains("checkpointing"));
    }

    #[tokio::test]
    async fn empty_and_unknown_queries_return_empty_not_errors() {
        let fx = fixture();
        let conn = control::connect(&fx.store).unwrap();
        let embedder = FakeEmbedder::new();
        let knowledge = knowledge_with_vectors(&fx, &embedder).await;
        let retriever = Retriever::new(
            &conn,
            &knowledge,
            &embedder,
            &WhatlangNormalizer,
            &IdentityReranker,
        );
        assert!(retriever.query("", 5).await.unwrap().is_empty());
    }
}
