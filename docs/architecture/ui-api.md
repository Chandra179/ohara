# Frontend and local API

The frontend is a separate client with one typed API boundary. It owns layout,
view state, loading/empty/error states, notifications, and user interaction.
It does not know about SQLite, LadybugDB, provider SDKs, filesystem paths, or
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
  fallback states.

The health response includes control-store, knowledge-store, embedder, and LLM
statuses. It also includes actionable diagnostics for unavailable components
and exposes `reranker: "identity"`, the deterministic operator-query baseline.

The query response carries explicit `availability` and `grounding` values. The
frontend consumes these values directly, so an unreachable language model is
not confused with an available model that returned malformed or uncited output.

The Overview, Documents, and Entities screens now consume read-only HTTP
models. Document status values are preserved from the control schema and the UI
does not invent a document type that the schema does not store. Entity review
previews show both candidates and their similarity score; merge mutations are
intentionally deferred until the lifecycle contract is ready.

## Readiness for live use

The P0 live integration slice and P1 read-only workflow now cover health,
metrics, overview, documents, query, entity reviews, and unavailable
model/store fixtures through Rust-backed browser checks.

Health checks now return actionable diagnostics for missing embedding files and
invalid knowledge artifacts. Health still probes the default local providers at
the transport boundary; a shared readiness seam should be introduced before
the API grows more provider-specific checks.
