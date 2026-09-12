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

## P1 — complete the read-only frontend workflow

- [x] Add paginated document and queue read models for Overview and Documents.
- [x] Align frontend document/status types with the control-plane schema; the
  control schema has no document-type field and supports more statuses than the
  current UI model.
- [x] Add entity-review listing and merge-preview read models for Entities.
- [x] Add live Rust-backed Playwright coverage for health, metrics, query,
  documents, and entity-review flows.
- [x] Keep URL registration as a library operation for this release; the
  ingestion UI and public URL-add workflow remain future work, so stale
  `ohara enqueue` references must not be reintroduced.

## P2 — API lifecycle and operational safety

- [ ] Add worker-process readiness and lifecycle reporting after the API can
  supervise or reliably observe ingestion.
- [ ] Add request limits, CORS policy, and local authentication/CSRF protection
  before exposing browser-triggered mutations.
- [ ] Add explicit lifecycle API contracts for requeue, archive, and delete,
  including confirmation, authorization, idempotency, and failure behavior.
- [ ] Add a shared readiness/health seam so transport handlers do not construct
  concrete provider implementations directly.
- [ ] Add the document/entity/lifecycle API handlers through public facades only;
  never expose SQL, graph queries, filesystem paths, or CLI subprocesses.

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
