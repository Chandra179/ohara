# Frontend and local API

The frontend is a separate client with one typed API boundary. It owns layout,
view state, loading/empty/error states, notifications, and user interaction.
It does not know about SQLite, Qdrant, FalkorDB, provider SDKs, filesystem paths, or
CLI subprocesses.

## Current shape

The mock adapter remains the default for offline design and unit tests. The HTTP
adapter is selected for the local development launcher and currently supports:

- health and service-status display;
- overview document counts and a bounded live queue projection;
- cursor-paginated document summaries with durable status and search filtering;
- pending entity-review candidates and read-only similarity previews;
- operator metrics for the Operations view; and
- query submission with grounded, ungrounded, unavailable, and ranked-source
  fallback states; and
- bounded topic discovery that normalizes, deduplicates, and queues article URLs.

The health response includes control-store, knowledge-store, embedder, LLM, and
worker statuses. Worker status includes the latest lifecycle state, boot and
heartbeat timestamps, current stage/job when available, and a stale flag.
Actionable diagnostics explain unavailable components or ingestion, and the
response exposes `reranker: "identity"`, the deterministic operator-query
baseline.

The query response carries explicit `availability` and `grounding` values. The
frontend consumes these values directly, so an unreachable language model is
not confused with an available model that returned malformed or uncited output.

The API owns a process-local query runtime. It lazily assembles the query
Adapters after the request path is allowed by readiness checks and reuses them
across successful requests. Construction failures are not cached, allowing a
repaired local model or knowledge artifact to be retried without restarting the
API.

The frontend displays worker readiness in the shared shell. A missing, stopped,
failed, or stale worker is shown separately from API dependency health so a
read-only query surface is not confused with ingestion availability.

## Topic discovery and queueing

`POST /api/topics/scrape` accepts `{ "topic": string, "limit": number? }`.
Topics are limited to 200 characters; the default result limit is 5 and the
allowed range is 1–10. The Engine searches Bing News RSS, validates HTTP(S)
article destinations, and returns provider-neutral results. The Control facade
registers each normalized URL idempotently and reports `enqueued` or
`duplicate`; the worker later performs the normal fetch-to-index pipeline.

The request body is bounded to 16 KiB. This is a local loopback mutation surface
for now. CORS, authentication, and CSRF controls are still required before
binding the API beyond loopback or adding broader browser-triggered mutations.

The Overview, Documents, and Entities screens consume typed HTTP models.
Document status values are preserved from the control schema and the UI does not
invent a document type that the schema does not store. Entity review previews
show both candidates and their similarity score; merge mutations are
intentionally deferred until the lifecycle contract is ready.

## Readiness for live use

The live integration slice covers health, metrics, overview, documents, topic
discovery/queueing, query, entity reviews, and unavailable model/store fixtures
through Rust-backed browser checks.

Health checks now return actionable diagnostics for missing embedding files and
invalid knowledge artifacts. Readiness probing is composed centrally and the
transport maps the provider-neutral report into the HTTP contract; adding a new
provider requires changing the readiness composition, not a route handler.
