# Retrieval module

Retrieval is the only frontend-facing Rust process. It exposes health, overview,
documents, metrics, topic proxying, entity-review placeholders, and query
routes. It reads catalog/index artifacts and never lets the browser access local
paths or provider credentials.

Health reports the five process states (`scraper`, `cleaning`, `indexer`,
`graph`, and `retrieval`) separately from provider states (`artifactStore`,
`qdrant`, `falkordb`, `embeddingModel`, and `ollama`). A stale stage heartbeat
is a degraded readiness diagnostic, not a hidden worker state.

Queries are embedded with the same model used by the indexer, sent to Qdrant,
and returned as ranked evidence. The bounded evidence is sent to the configured
Ollama model (`phi4-mini:latest` by default). A non-empty synthesis is marked
grounded and cites the returned chunk ids; missing model output remains an
explicit unavailable or ungrounded result.

The retrieval process does not run ingestion. Topic requests are forwarded to
the scraper process, which makes the process seam visible and independently
operable. Its metrics route also exposes the durable counters and latency
snapshots written by every process.

For deterministic process-boundary tests, retrieval accepts the same
`OHARA_EMBEDDING_MODE=deterministic` setting as the indexer. This setting is
test-only; normal operation uses the cached `bge-small-en-v1.5` model.
