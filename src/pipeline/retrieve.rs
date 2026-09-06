//! Stage 5 — Retrieval (§8): query normalization, three-path fusion, rerank,
//! synthesis — and its two ports, [`QueryNormalizer`] and [`Reranker`] (§1.3).
//! The pipeline implementation lands with the §15 step 5 build step.

use async_trait::async_trait;

use crate::Class;

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
#[derive(Debug, Clone)]
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
