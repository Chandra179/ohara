//! Stage 5 — Retrieval (§8), full three-path form: query normalization → query
//! entities → BM25 + vector + graph candidate paths → Reciprocal Rank Fusion →
//! rerank with degradation. Its two ports, [`QueryNormalizer`] and
//! [`Reranker`] (§1.3), plus the baseline implementations.
//!
//! §8 Stage 5 items that remain for later steps: symspell domain-dictionary
//! correction and optional `HyDE`. Citation-preserving synthesis is owned by
//! [`super::synthesis`] after this module assembles the retrieval context.
//!
//! The graph path (§8 Stage 5.2–5.3) runs whenever the knowledge store declares
//! `graph_traversal` — query entities come from typed aliases plus
//! `EntityNames` embedding KNN, and their `:MENTIONS`-linked chunks join the
//! fusion pool as a third list. A store without the capability degrades to the
//! two-path baseline: the query never fails on a missing path.

use async_trait::async_trait;

use crate::Class;
use crate::config::RetrievalConfig;
use crate::control::{self, ControlDb, DbError};
use crate::knowledge::{Fact, KnowledgeError, KnowledgeStore, ModelId, ScoredHit, VectorSpace};
use crate::pipeline::Embedder;
use crate::text::normalize_surface_form;

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

/// A chunk candidate with a relevance score — the reranker's pool currency (§8
/// Stage 5: fusion produces the configured pool, rerank returns requested top-k).
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredChunk {
    /// The chunk's cross-store identity (`sha256(doc_id:seq)`, §3).
    pub chunk_id: String,
    /// Display text.
    pub text: String,
    /// Current relevance score (fused rank or reranker score).
    pub score: f32,
}

/// Retrieved material passed to Stage 5.6 synthesis: ranked chunks plus the
/// bounded graph facts connected to query entities.
#[derive(Debug, Clone, PartialEq)]
pub struct RetrievedContext {
    /// Ranked chunk sources, including immutable `chunk_id` citations.
    pub chunks: Vec<ScoredChunk>,
    /// Labeled graph facts near the query entities.
    pub facts: Vec<Fact>,
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
/// Below the configured confidence floor the result is treated as English — the
/// Stage 2 gate already bounds this tool's corpora to `target_languages`, and
/// [`Lang`] here is metadata for callers, never a hard gate on the query path.
pub struct WhatlangNormalizer {
    confidence_floor: f64,
}

impl WhatlangNormalizer {
    /// Builds a normalizer with the configured language-detection confidence
    /// floor. Configuration owns this behavior-changing threshold.
    #[must_use]
    pub fn new(confidence_floor: f64) -> Self {
        Self { confidence_floor }
    }
}

impl QueryNormalizer for WhatlangNormalizer {
    fn normalize(&self, query: &str) -> (Lang, String) {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return (Lang::Other, String::new());
        }
        let lang = match whatlang::detect(trimmed) {
            Some(info) if info.confidence() >= self.confidence_floor => {
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
                .then_with(|| a.text.cmp(&b.text))
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
                .then_with(|| a.text.cmp(&b.text))
                .then_with(|| a.chunk_id.cmp(&b.chunk_id))
        });
        Ok(reranked)
    }
}

/// The fusion pool: each list contributes `1/(k + rank)` per candidate (§8) —
/// any number of paths, best first.
fn rrf_fuse(lists: Vec<Vec<(String, String)>>, rrf_k: f32) -> Vec<ScoredChunk> {
    let mut scores: std::collections::HashMap<String, (f32, String)> =
        std::collections::HashMap::new();
    for list in lists {
        for (rank, (chunk_id, text)) in list.into_iter().enumerate() {
            let entry = scores
                .entry(chunk_id)
                .or_insert_with(|| (0.0, text.clone()));
            entry.0 += 1.0 / (rrf_k + f32::from(u16::try_from(rank).unwrap_or(u16::MAX)) + 1.0);
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
            .then_with(|| a.text.cmp(&b.text))
            .then_with(|| a.chunk_id.cmp(&b.chunk_id))
    });
    fused
}

/// One query entity resolved from the query text (§8 Stage 5.2) — an entity id
/// with the evidence that surfaced it.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryEntity {
    /// The resolved entity id.
    pub entity_id: String,
    /// How the entity was found: `alias` (exact typed-alias hit) or `embedding`
    /// (`EntityNames` KNN above the similarity floor).
    pub source: QueryEntitySource,
    /// The similarity score for embedding hits; 1.0 for exact alias hits.
    pub score: f32,
}

/// How a [`QueryEntity`] was surfaced (§8 Stage 5.2) — metrics/audit metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryEntitySource {
    /// Exact typed-alias hit on `entity_aliases`.
    Alias,
    /// `EntityNames` embedding KNN above the similarity floor.
    Embedding,
}

/// The retrieval engine (§8 Stage 5): three candidate paths over the stores,
/// fusion, rerank. Ports are injected (§9); the connection borrows the caller's
/// control store. `query` is async — its only blocking work is one query
/// embedding (ms-scale, unlike the worker's batch path, §9) and the `SQLite`
/// BM25 scan.
pub struct Retriever<'a> {
    conn: &'a ControlDb,
    knowledge: &'a dyn KnowledgeStore,
    embedder: &'a dyn Embedder,
    normalizer: &'a dyn QueryNormalizer,
    reranker: &'a dyn Reranker,
    read_model: ModelId,
    retrieval: RetrievalConfig,
}

impl<'a> Retriever<'a> {
    /// Builds a retriever over the configured read namespace and `[retrieval]`
    /// knobs; must be called inside a tokio runtime (the vector and graph paths
    /// drive the async [`KnowledgeStore`] port from this sync context via the
    /// captured handle).
    #[must_use]
    pub fn new(
        conn: &'a ControlDb,
        knowledge: &'a dyn KnowledgeStore,
        embedder: &'a dyn Embedder,
        normalizer: &'a dyn QueryNormalizer,
        reranker: &'a dyn Reranker,
        read_model: ModelId,
        retrieval: RetrievalConfig,
    ) -> Self {
        Self {
            conn,
            knowledge,
            embedder,
            normalizer,
            reranker,
            read_model,
            retrieval,
        }
    }

    /// Runs one query (§8 Stage 5): normalize → query entities → BM25 + vector
    /// + graph paths → RRF fusion → rerank with degradation → top `top_k`.
    ///
    /// # Errors
    /// [`RetrieveError`] — the rerank step degrades to fusion order instead
    /// (§8 Stage 5.5); everything else propagates.
    pub async fn query(
        &self,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<ScoredChunk>, RetrieveError> {
        Ok(self.query_context(query, top_k).await?.chunks)
    }

    /// Retrieves ranked chunks and graph facts for Stage 5.6 synthesis.
    ///
    /// The graph entity resolution is performed once and reused for both the
    /// graph candidate path and fact context. Facts are sorted by their stable
    /// identity so provider prompts do not depend on backend iteration order.
    ///
    /// # Errors
    /// [`RetrieveError`] when a required retrieval path or graph read fails.
    pub async fn query_context(
        &self,
        query: &str,
        top_k: usize,
    ) -> Result<RetrievedContext, RetrieveError> {
        let (_lang, normalized) = self.normalizer.normalize(query);

        // Path 1 — BM25 via the trigger-synced chunks_fts (§8: embeddings are
        // weak on exact identifiers like "SQLite").
        let bm25 = self.bm25_path(&normalized)?;

        // Path 2 — vector KNN over the read-model collection (§4's atomic read
        // switch).
        let vector = self.vector_path(&normalized).await?;

        // Path 3 — graph (§8 Stage 5.3): chunks that mention the query's
        // entities. Capability-gated: a store without graph traversal keeps
        // the two-path baseline rather than failing the query.
        let graph_enabled = self.knowledge.capabilities().graph_traversal;
        let entities = if graph_enabled {
            self.query_entities(&normalized).await?
        } else {
            Vec::new()
        };
        let mut paths = vec![bm25, vector];
        if graph_enabled {
            paths.push(self.graph_path(&entities).await?);
        }

        // Fusion (§8 Stage 5.4): configured RRF → configured candidate pool.
        let pool: Vec<ScoredChunk> = rrf_fuse(paths, self.retrieval.rrf_k())
            .into_iter()
            .take(self.retrieval.pool())
            .collect();

        // Rerank with degradation (§8 Stage 5.5): on RerankError the fusion
        // order is returned as-is — the query never fails on the reranker.
        let ranked = match self.reranker.rerank(query, pool.clone()).await {
            Ok(reranked) => reranked,
            Err(_degraded) => pool,
        };
        let chunks = ranked.into_iter().take(top_k).collect();
        let mut facts = if graph_enabled && !entities.is_empty() {
            let ids: Vec<&str> = entities.iter().map(|e| e.entity_id.as_str()).collect();
            self.knowledge
                .facts_within_hops(&ids, self.retrieval.fact_hops())
                .await?
        } else {
            Vec::new()
        };
        facts.sort_by(|a, b| {
            a.subject_id
                .cmp(&b.subject_id)
                .then_with(|| a.predicate.as_str().cmp(b.predicate.as_str()))
                .then_with(|| a.object_id.cmp(&b.object_id))
        });
        Ok(RetrievedContext { chunks, facts })
    }

    /// Query entities (§8 Stage 5.2): each normalized query term is looked up
    /// as a typed alias (a homograph contributes all its type-variants), then
    /// the whole query is matched against the `EntityNames` collection above
    /// the embedding floor. Exact alias hits rank ahead of embedding hits; the
    /// combined list caps at `max_query_entities`.
    ///
    /// # Errors
    /// [`RetrieveError`] on store or embedding failure.
    pub async fn query_entities(
        &self,
        normalized: &str,
    ) -> Result<Vec<QueryEntity>, RetrieveError> {
        if normalized.trim().is_empty() {
            return Ok(Vec::new());
        }

        // Pass 1 — exact typed-alias hits over query phrases (§8: a homograph
        // yields every entity; disambiguation moves downstream). Entity
        // surface names are often multi-word ("wal checkpointing"), so the
        // pass tries every phrase window, longest first — a longer window
        // ranks ahead of its fragments' windows in the alias-first ordering,
        // and `seen` keeps one row per entity regardless of how many phrases
        // hit it. Terms are normalized the way stored aliases were (§8
        // Stage 4.1).
        let mut aliases: Vec<QueryEntity> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let terms: Vec<String> = normalized
            .split_whitespace()
            .map(normalize_surface_form)
            .collect();
        for window in (1..=terms.len()).rev() {
            for start in 0..=terms.len().saturating_sub(window) {
                let phrase = terms[start..start + window].join(" ");
                for entity_id in control::lookup_alias_all_types(self.conn, &phrase)? {
                    if seen.insert(entity_id.clone()) {
                        aliases.push(QueryEntity {
                            entity_id,
                            source: QueryEntitySource::Alias,
                            score: 1.0,
                        });
                    }
                }
            }
        }

        // Pass 2 — `EntityNames` embedding KNN above the floor. The collection
        // spans supertypes; query entities deliberately do not filter by type —
        // the query names no type, so all variants ride along and rerank and
        // graph context do the disambiguating.
        let mut embedding: Vec<QueryEntity> = Vec::new();
        let embedded = self.embedder.embed(&[normalized])?;
        if let Some(q) = embedded.into_iter().next() {
            let floor = self.retrieval.entity_embedding_threshold();
            let hits = self
                .knowledge
                .knn(
                    VectorSpace::EntityNames,
                    &q,
                    self.retrieval.max_query_entities() * 2,
                    &crate::knowledge::ChunkFilter {},
                )
                .await?;
            for ScoredHit { id, score } in hits {
                if f64::from(score) >= floor && seen.insert(id.clone()) {
                    embedding.push(QueryEntity {
                        entity_id: id,
                        source: QueryEntitySource::Embedding,
                        score,
                    });
                }
            }
        }
        embedding.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.entity_id.cmp(&b.entity_id))
        });

        let mut out = aliases;
        out.extend(embedding);
        out.truncate(self.retrieval.max_query_entities());
        Ok(out)
    }

    /// Graph path (§8 Stage 5.3): chunks whose `:MENTIONS` edges connect to
    /// the query's entities. Best-first order is mention-then-chunk-id — the
    /// store returns an unordered set; RRF needs a deterministic sequence.
    ///
    /// # Errors
    /// [`RetrieveError`] on store or embedding failure.
    async fn graph_path(
        &self,
        entities: &[QueryEntity],
    ) -> Result<Vec<(String, String)>, RetrieveError> {
        if entities.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<&str> = entities.iter().map(|e| e.entity_id.as_str()).collect();
        let chunk_ids = self.knowledge.chunks_for_entities(&ids).await?;
        if chunk_ids.is_empty() {
            return Ok(Vec::new());
        }
        // Deterministic order (RRF ranks by position): hydrate and sort by
        // stable content first, then identity. The store's set semantics make
        // its output order arbitrary, and UUID-derived chunk ids are not stable
        // between fresh evaluation fixtures.
        let mut sorted_ids = chunk_ids;
        sorted_ids.sort();
        sorted_ids.dedup();
        let ids: Vec<&str> = sorted_ids.iter().map(String::as_str).collect();
        let mut hydrated = control::chunks_by_ids(self.conn, &ids)?;
        hydrated.sort_by(|a, b| {
            a.text
                .cmp(&b.text)
                .then_with(|| a.chunk_id.cmp(&b.chunk_id))
        });
        Ok(hydrated
            .into_iter()
            .map(|chunk| (chunk.chunk_id, chunk.text))
            .collect())
    }

    /// BM25 path (§8 Stage 5.3): `chunks_fts` `MATCH`, bm25 order, best first.
    fn bm25_path(&self, normalized: &str) -> Result<Vec<(String, String)>, RetrieveError> {
        let expression = fts_match_expression(normalized);
        if expression.is_empty() {
            return Ok(Vec::new());
        }
        Ok(
            control::search_bm25(self.conn, &expression, self.retrieval.pool())?
                .into_iter()
                .map(|chunk| (chunk.chunk_id, chunk.text))
                .collect(),
        )
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
            model_id: self.read_model.clone(),
        };
        let hits = self
            .knowledge
            .knn(
                space,
                &q,
                self.retrieval.pool(),
                &crate::knowledge::ChunkFilter {},
            )
            .await?;
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        let hydrated = control::chunks_by_ids(self.conn, &ids)?;
        let by_id: std::collections::HashMap<String, String> =
            hydrated.into_iter().map(|c| (c.chunk_id, c.text)).collect();
        // KNN score is the ranking; stable content breaks equal-score ties
        // before the cross-store identity. This prevents backend iteration
        // order from changing evaluation results between fresh fixtures.
        let mut ranked: Vec<(String, String, f32)> = hits
            .into_iter()
            .filter_map(|hit| {
                by_id
                    .get(&hit.id)
                    .map(|text| (hit.id, text.clone(), hit.score))
            })
            .collect();
        ranked.sort_by(|a, b| {
            b.2.total_cmp(&a.2)
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.0.cmp(&b.0))
        });
        Ok(ranked
            .into_iter()
            .map(|(chunk_id, text, _score)| (chunk_id, text))
            .collect())
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp
)] // exact-value assertions
mod tests {
    use std::sync::Arc;

    use crate::config::Config;
    use crate::control::testing::seed_doc;
    use crate::control::{self, ControlDb, NewChunkRow};
    use crate::knowledge::{InMemoryKnowledge, ModelId, VectorSpace};
    use crate::pipeline::test_support::FakeEmbedder;
    use crate::text::sha256_hex;

    use super::{
        IdentityReranker, QueryEntitySource, Retriever, WhatlangNormalizer, fts_match_expression,
    };
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
        let n = WhatlangNormalizer::new(0.5);
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
        /// Per-doc chunk ids (`sha256(doc_id:seq)`, §3), seq order.
        chunk_ids: Vec<Vec<String>>,
    }

    /// Seeds two documents with three chunks each; chunk texts carry distinct
    /// keywords so BM25 and the fake-embedder vector path both resolve them.
    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("data")).unwrap();
        let toml_path = dir.path().join("ohara.toml");
        std::fs::write(
            &toml_path,
            format!(
                "data_dir = {:?}\n\
[embedder]\n\
model_id = \"fake-embedder\"\n\
dim = 4\n\
[knowledge]\n\
read_model = \"fake-embedder\"\n\
write_model = \"fake-embedder\"\n",
                dir.path().join("data").display()
            ),
        )
        .unwrap();
        let config = Arc::new(Config::load(Some(&toml_path)).unwrap());
        let store = dir.path().join("store.db");
        let conn = control::connect(&store).unwrap();
        let mut doc_ids = Vec::new();
        let mut chunk_ids: Vec<Vec<String>> = Vec::new();
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
            chunk_ids.push(rows.iter().map(|r| r.chunk_id.clone()).collect());
            control::replace_chunks(&conn, &doc_id, &rows).unwrap();
        }
        drop(conn);
        Fixture {
            dir,
            config,
            store,
            doc_ids,
            chunk_ids,
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
                    .raw()
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
        let normalizer =
            WhatlangNormalizer::new(fx.config.retrieval().detection_confidence_floor());
        let retriever = Retriever::new(
            &conn,
            &knowledge,
            &embedder,
            &normalizer,
            &IdentityReranker,
            ModelId::new(fx.config.knowledge().read_model()),
            fx.config.retrieval().clone(),
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
        let normalizer =
            WhatlangNormalizer::new(fx.config.retrieval().detection_confidence_floor());
        let retriever = Retriever::new(
            &conn,
            &knowledge,
            &embedder,
            &normalizer,
            &BrokenReranker,
            ModelId::new(fx.config.knowledge().read_model()),
            fx.config.retrieval().clone(),
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
        let normalizer =
            WhatlangNormalizer::new(fx.config.retrieval().detection_confidence_floor());
        let retriever = Retriever::new(
            &conn,
            &knowledge,
            &embedder,
            &normalizer,
            &IdentityReranker,
            ModelId::new(fx.config.knowledge().read_model()),
            fx.config.retrieval().clone(),
        );
        assert!(retriever.query("", 5).await.unwrap().is_empty());
    }

    /// The graph-path fixture: the §8 Stage 4 write shape, seeded directly —
    /// registry entities + aliases in `SQLite`, nodes + name vectors +
    /// `:MENTIONS` edges in the knowledge store.
    async fn graph_fixture() -> (Fixture, ControlDb, FakeEmbedder, InMemoryKnowledge) {
        let fx = fixture();
        let conn = control::connect(&fx.store).unwrap();
        let embedder = FakeEmbedder::new();
        let knowledge = InMemoryKnowledge::default();

        // One entity ("sqlite", PRODUCT) mentioned by doc 0's first chunk.
        control::ensure_entity(&conn, "ent-sqlite", "sqlite", "PRODUCT", None).unwrap();
        control::upsert_alias(&conn, "sqlite", "PRODUCT", "ent-sqlite").unwrap();
        knowledge
            .upsert_entity(&crate::knowledge::EntityRecord {
                entity_id: "ent-sqlite".to_string(),
                canonical_name: "sqlite".to_string(),
                entity_type: crate::knowledge::EntityType::Product,
                subtype: None,
            })
            .await
            .unwrap();
        let name_vec = embedder.embed(&["sqlite"]).unwrap().remove(0);
        knowledge
            .upsert_vectors(VectorSpace::EntityNames, "", &["ent-sqlite"], &[name_vec])
            .await
            .unwrap();
        knowledge
            .link_mention(&fx.chunk_ids[0][0], "ent-sqlite")
            .await
            .unwrap();

        // A homograph: "checkpointing" is both a CONCEPT and an EVENT, each
        // mentioned by a different chunk (§8 Stage 5.2 disambiguation shape).
        let ckpt_concept_chunk = fx.chunk_ids[0][1].clone();
        let ckpt_event_chunk = fx.chunk_ids[1][0].clone();
        for (id, etype, mention) in [
            (
                "ent-ckpt-c",
                crate::knowledge::EntityType::Concept,
                &ckpt_concept_chunk,
            ),
            (
                "ent-ckpt-e",
                crate::knowledge::EntityType::Event,
                &ckpt_event_chunk,
            ),
        ] {
            control::ensure_entity(&conn, id, "checkpointing", etype.as_str(), None).unwrap();
            control::upsert_alias(&conn, "checkpointing", etype.as_str(), id).unwrap();
            knowledge
                .upsert_entity(&crate::knowledge::EntityRecord {
                    entity_id: id.to_string(),
                    canonical_name: "checkpointing".to_string(),
                    entity_type: etype,
                    subtype: None,
                })
                .await
                .unwrap();
            knowledge.link_mention(mention, id).await.unwrap();
        }
        (fx, conn, embedder, knowledge)
    }

    #[tokio::test]
    async fn query_entities_resolve_aliases_and_embeddings() {
        let (fx, conn, embedder, knowledge) = graph_fixture().await;
        let normalizer =
            WhatlangNormalizer::new(fx.config.retrieval().detection_confidence_floor());
        let retriever = Retriever::new(
            &conn,
            &knowledge,
            &embedder,
            &normalizer,
            &IdentityReranker,
            ModelId::new(fx.config.knowledge().read_model()),
            fx.config.retrieval().clone(),
        );

        // Exact alias hit — one term, one entity, all type-variants. Embedding
        // hits may follow (the fake's vectors are coarse); the alias hit leads.
        let entities = retriever.query_entities("sqlite").await.unwrap();
        let first = entities.first().unwrap();
        assert_eq!(first.entity_id, "ent-sqlite");
        assert_eq!(first.source, QueryEntitySource::Alias);
        assert_eq!(first.score, 1.0); // exact-alias hits carry the constant 1.0
        assert!(entities.len() <= fx.config.retrieval().max_query_entities());

        // Homograph: every type-variant comes back (§8 Stage 5.2). The fake
        // embedder's coarse vectors may also surface embedding hits — the
        // invariant under test is that both alias variants are present.
        let entities = retriever.query_entities("checkpointing").await.unwrap();
        let ids: Vec<&str> = entities.iter().map(|e| e.entity_id.as_str()).collect();
        assert!(
            ids.contains(&"ent-ckpt-c") && ids.contains(&"ent-ckpt-e"),
            "got {ids:?}"
        );
        assert!(
            entities.iter().all(|e| e.source == QueryEntitySource::Alias
                || e.source == QueryEntitySource::Embedding)
        );

        // Embedding pass: the fake embedder gives "sqlite" a word-hash vector;
        // a query sharing vocabulary lands above the floor and the entity
        // resolves via `EntityNames` KNN. The embedding threshold in the
        // fixture's default config is 0.75; cosine of identical vectors is 1.
        let entities = retriever.query_entities("sqlite database").await.unwrap();
        assert!(entities.iter().any(|e| e.entity_id == "ent-sqlite"));

        // Unknown terms resolve nothing.
        assert!(
            retriever
                .query_entities("nothing matches")
                .await
                .unwrap()
                .is_empty()
        );
        drop(fx);
    }

    #[tokio::test]
    async fn graph_path_surfaces_mentioning_chunks_into_fusion() {
        let (fx, conn, embedder, knowledge) = graph_fixture().await;
        let normalizer =
            WhatlangNormalizer::new(fx.config.retrieval().detection_confidence_floor());
        let retriever = Retriever::new(
            &conn,
            &knowledge,
            &embedder,
            &normalizer,
            &IdentityReranker,
            ModelId::new(fx.config.knowledge().read_model()),
            fx.config.retrieval().clone(),
        );

        // The graph path is reachable through the public query: "sqlite" hits
        // the alias, `:MENTIONS` links doc 0's first chunk, and that chunk
        // competes in fusion on equal footing with the lexical paths.
        let hits = retriever.query("sqlite", 5).await.unwrap();
        assert!(
            hits.iter().any(|h| h.chunk_id == fx.chunk_ids[0][0]),
            "the mentioning chunk must be a candidate, got {hits:?}"
        );

        // A query naming no entity still works through the lexical paths.
        let hits = retriever.query("wal truncation", 5).await.unwrap();
        assert!(hits[0].text.contains("truncation"));

        // The graph path degrades gracefully for a store without traversal
        // (§9 honest capabilities): the lexical paths still answer. This store
        // has no chunk vectors either — the BM25 path alone carries the query.
        let knowledge = InMemoryKnowledge::without_graph();
        let normalizer =
            WhatlangNormalizer::new(fx.config.retrieval().detection_confidence_floor());
        let retriever = Retriever::new(
            &conn,
            &knowledge,
            &embedder,
            &normalizer,
            &IdentityReranker,
            ModelId::new(fx.config.knowledge().read_model()),
            fx.config.retrieval().clone(),
        );
        let hits = retriever.query("wal truncation", 5).await.unwrap();
        assert!(!hits.is_empty(), "the lexical paths must still answer");
        drop(fx);
    }
}
