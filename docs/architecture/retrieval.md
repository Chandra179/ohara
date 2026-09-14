# Retrieval module

Retrieval is the only frontend-facing Rust process. It exposes health, overview,
documents, metrics, topic proxying, entity-review placeholders, and query
routes. It reads catalog/index artifacts and never lets the browser access local
paths or provider credentials.

Health reports the five process states (`scraper`, `cleaning`, `indexer`,
`graph`, and `retrieval`) separately from provider states (`artifactStore`,
`qdrant`, `falkordb`, `embeddingModel`, and `ollama`). A stale stage heartbeat
is a degraded readiness diagnostic, not a hidden worker state.

Queries are embedded with the same model used by the indexer. Retrieval merges
three read-only signals before returning ranked evidence:

1. Qdrant cosine similarity over indexed chunk vectors.
2. Full-text term overlap over indexed chunk text and titles.
3. A bounded FalkorDB `Chunk` → `Entity` path lookup, scored when an entity
   name matches a query term.

Qdrant search is selected with `OHARA_QDRANT_SEARCH_MODE=exact|hnsw` and uses
the configured `OHARA_QDRANT_COLLECTION` (default `ohara_chunks`). Exact mode
sends Qdrant's `exact` search flag; HNSW mode sends the configured
`OHARA_QDRANT_HNSW_EF` value. Retrieval validates that the collection is a
384-dimensional single-vector collection before reporting it as ready. The
indexer and retrieval processes must use the same collection and embedding
configuration; changing either requires a rebuild or a separately populated
collection.

The signals are fused with the default weights vector `0.70`, full text `0.20`,
and graph path `0.10`. The response includes the fused score and a `signals`
object that reports which sources were available. Full-text and graph signals
are optional at query time: an unavailable FalkorDB or an empty local index
does not hide otherwise healthy Qdrant results. The bounded evidence is sent to
the configured Ollama model (`phi4-mini:latest` by default). A non-empty
synthesis is marked grounded and cites unique chunk ids from the returned
evidence. A grounded response must have non-empty answer text and citations
that refer only to unique returned chunks. Empty, malformed, or unavailable
model output remains an explicit ungrounded result with no citations; an
unavailable model is also reported through `availability`.

The graph lookup is read-only and capped to protect query latency. It uses the
current graph shape and is intentionally separate from entity extraction. The
graph process owns typed extraction and entity resolution; its threshold
measurement is a deterministic offline quality gate and does not change this
retrieval signal or the indexed artifact contract.

The retrieval process does not run ingestion. Topic requests are forwarded to
the scraper process, which makes the process seam visible and independently
operable. If `OHARA_PROCESS_AUTH_TOKEN` is configured, retrieval sends it as a
bearer token on that process-to-process request. Use the same secret on the
scraper and protect the connection with HTTPS or a private network. Its
metrics route also exposes the durable counters and latency snapshots written
by every process.

For deterministic process-boundary tests, retrieval accepts the same
`OHARA_EMBEDDING_MODE=deterministic` setting as the indexer. This setting is
test-only; normal operation uses the cached `bge-small-en-v1.5` model.

The golden retrieval evaluation uses a versioned fixture with labeled chunk
relevance. `make retrieval-quality` starts the real retrieval process against
local deterministic provider doubles, verifies non-empty grounded answers
and exact citations, and reports macro recall@1/3/5, MRR, and nDCG@1/3/5. The
fixture uses deterministic exact query/document matches so it is suitable for
CI; it validates the ranking response and metric contract, not production
semantic quality. A production-quality gate needs a labeled corpus collected
from representative user queries.

`make pipeline-fixture` additionally sends empty, unavailable, and malformed
Ollama responses through the retrieval HTTP Interface and checks that each is
returned as an explicit ungrounded result with no citations. The frontend
contract browser suite separately rejects duplicate citations, unknown chunk
ids, and citations without returned evidence.
