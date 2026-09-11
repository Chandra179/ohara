# Retrieval and evaluation

Retrieval combines complementary signals and preserves a useful degraded result
at every optional boundary.

## Query path

1. Normalize and language-check the query.
2. Resolve typed query entities through aliases and entity-name vectors.
3. Collect BM25, exact vector KNN, and graph-mention candidates.
4. Fuse lists with reciprocal-rank fusion.
5. Rerank the bounded pool; if reranking fails, retain fusion order.
6. Assemble bounded chunks and graph facts for structured synthesis.
7. Accept only non-empty answers citing immutable evidence ids; otherwise return
   ranked chunks.

The current machinery evaluator reports recall@20 for each path, fused MRR, and
reranking change. Its baseline is recall@20 = 1.000 per path, fused MRR = 0.723,
and rerank delta = 0.000. These are regression measurements, not production
quality claims.

The identity reranker is currently selected by the operator query assembly. The
local ONNX reranker exists behind its feature gate but is not yet wired into that
path by default.

## Open quality work

Symspell correction and HyDE remain unevaluated because they change query text,
latency, or both. Entity-resolution and retrieval similarity thresholds are
validated configuration defaults, not measured conclusions. Threshold work
must use entity-aware queries, graph-path coverage, ambiguity cases, and a
review-cost metric before changing defaults.

HNSW, if introduced, must meet a recall gate against exact KNN before it becomes
the default. Its search parameters are implementation details of that future
adapter, not current runtime behavior.
