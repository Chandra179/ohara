# TODO

Prioritized implementation queue for the core pipeline and its local operator
surface. The [system architecture](docs/ARCHITECTURE.md) describes the target
boundaries; this file records what is still open.

## P0 — release-blocking correctness

- [x] Fix the nested `llmUsage` metrics DTO to use the frontend's camelCase
  contract and add a Rust-route plus browser regression test. The live
  Operations view currently crashes when it receives the server response.
- [x] Preserve the distinction between unavailable LLM synthesis and an
  ungrounded answer in the HTTP adapter and API response contract.
- [x] Add actionable readiness diagnostics for missing embedding models and
  invalid knowledge-store artifacts; every frontend route must show an error
  state instead of a blank page.
- [x] Keep the identity reranker as the selected operator-query baseline,
  explicitly document it, and expose the selection in the API and query view.
- [x] Recover a truncated Ladybug WAL checkpoint tail when the base knowledge
  index remains valid, and leave non-recoverable artifacts actionable.

## P1 — complete the local frontend workflow

- [x] Add paginated document and queue read models for Overview and Documents.
- [x] Align frontend document/status types with the control-plane schema; the
  control schema has no document-type field and supports more statuses than the
  current UI model.
- [x] Add entity-review listing and merge-preview read models for Entities.
- [x] Add live Rust-backed Playwright coverage for health, metrics, query,
  documents, and entity-review flows.
- [x] Add bounded topic discovery and queueing from the Overview screen with
  normalized URLs, duplicate reporting, and Rust-backed browser coverage.

## P2 — API lifecycle and operational safety

- [x] Keep successful query provider construction process-local and lazy so
  HTTP requests reuse the same behavioral ports without making API startup
  fail before readiness diagnostics can be shown.
- [x] Add worker-process readiness and lifecycle observation through durable
  control-plane heartbeats; API supervision remains out of scope.
- [ ] Extend the current bounded, loopback-only topic mutation with CORS policy
  and local authentication/CSRF protection before exposing the API beyond
  loopback or adding broader browser-triggered mutations.
- [ ] Add explicit lifecycle API contracts for requeue, archive, and delete,
  including confirmation, authorization, idempotency, and failure behavior.
- [x] Add a shared readiness/health seam so transport handlers do not construct
  concrete provider implementations directly.
- [x] Keep default provider assembly in the runtime composition root so the
  pipeline names ports rather than concrete network or datastore adapters.
- [x] Keep document and entity read handlers on public facades; transport code
  does not expose SQL, graph queries, filesystem paths, or CLI subprocesses.
- [x] Move the Ollama HTTP adapter into the Engine plane and keep the LLM port
  provider-neutral.
- [x] Split the Stage 4 extraction/graph, entity merge/review, and HTTP health
  and error implementations into focused modules.
- [x] Replace the bespoke query encoder with the `url` crate serializer and add
  encoded-key regression coverage.
- [x] Add Rust and frontend quality gates to repository CI.

## P3 — retrieval and entity-resolution quality

- [ ] Measure ER name and embedding thresholds on entity-aware, ambiguous, and
  cross-document golden-set cases; report precision, recall, review volume, and
  merge error cost before changing defaults.
- [ ] Expand retrieval evaluation with graph-path recall, multi-hop context,
  duplicate/deletion cases, quality-gate cases, and failure/retry cases.
- [ ] Evaluate Symspell correction and HyDE independently for quality, latency,
  and query-rewrite regressions before enabling either.
- [ ] Add HNSW behind the knowledge port only after benchmarking it against exact
  KNN and gating adoption on recall.
- [ ] Add stage throughput and latency metrics, then expose them through an
  operator dashboard/export contract.

## P4 — providers and migrations

- [ ] Add cloud LLM providers behind the `Llm` port with explicit opt-in egress,
  usage accounting, and contract tests.
- [ ] Implement and acceptance-test embedding dual-write migration before
  allowing different read and write model namespaces.
- [ ] Extend the Fetcher contract suite for each additional provider or ladder
  leg.

## Completed baseline

- [x] Implement the local pipeline stages, recovery, leases, retry/dead-letter
  handling, and audit events.
- [x] Implement the fetch ladder, URL normalization, SSRF/robots/politeness
  policy, browser-profile leg, and optional Obscura adapter.
- [x] Implement exact vector retrieval, BM25/vector/graph fusion, graph
  extraction, typed entity resolution, and offline ER merge.
- [x] Implement citation-preserving local synthesis, fallback behavior, durable
  LLM usage accounting, and the `query` operator command.
- [x] Implement staged backups, lifecycle operators, entity GC, raw retention,
  and read-only operator metrics.
- [x] Implement the initial loopback API for health, metrics, and query plus the
  typed frontend HTTP adapter and its contract tests.
- [x] Remove the unsupported frontend entity-merge mutation from the read-only
  interface until the lifecycle contract is implemented.
- [x] Align component documentation with the current read-only API and live
  browser coverage.
