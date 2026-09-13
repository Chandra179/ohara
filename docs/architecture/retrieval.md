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

The signals are fused with the default weights vector `0.70`, full text `0.20`,
and graph path `0.10`. The response includes the fused score and a `signals`
object that reports which sources were available. Full-text and graph signals
are optional at query time: an unavailable FalkorDB or an empty local index
does not hide otherwise healthy Qdrant results. The bounded evidence is sent to
the configured Ollama model (`phi4-mini:latest` by default). A non-empty
synthesis is marked grounded and cites the returned chunk ids; missing model
output remains an explicit unavailable or ungrounded result.

The graph lookup is read-only and capped to protect query latency. It uses the
current graph shape and is intentionally separate from entity extraction;
structured extraction and typed entity resolution remain a later priority.

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
local standard-library provider doubles, verifies non-empty grounded answers
and exact citations, and reports macro recall@1/3/5, MRR, and nDCG@1/3/5. The
fixture uses deterministic exact query/document matches so it is suitable for
CI; it validates the ranking response and metric contract, not production
semantic quality. A production-quality gate needs a labeled corpus collected
from representative user queries.
