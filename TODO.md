# TODO

Work remaining to bring the pipeline to the full GraphRAG architecture described
in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). Build-order references are §15
steps. Anything not listed here is implemented (or a deliberate, documented
non-goal).

## Completed stabilization coverage

The following safeguards are implemented and covered before feature expansion:

- [x] The retrieval evaluator records BM25, vector, and graph recall@20, fused MRR,
  rerank delta, and named regression floors.
- [x] Acceptance cases cover typed entity retrieval, one- and two-hop graph context,
  duplicate content surviving sibling deletion, and cleanup across FTS, vectors,
  and graph mentions.
- [x] Quality-gate coverage includes wrong-language and paywall rejection without
  chaining VECTORIZE.
- [x] Boot/restart reconciliation, expired-lease replay, retry-to-dead-letter
  classification, and `requeue` recovery are covered by the worker suite.
- [x] The `Embedder` port exposes provider input capacity; worker boot rejects a
  chunk budget that would exceed it, preventing silent provider truncation.
- [x] `ohara query` synthesizes a bounded answer from top chunks and labeled graph
  facts with exact immutable citations, and falls back to ranked chunks when the
  LLM is unavailable or ungrounded. `ohara backup` writes a staged,
  non-overwriting snapshot behind the worker runtime lock.
- [x] `ohara requeue --doc <id>` resets failed/interrupted jobs, `ohara archive <id>`
  preserves queryable chunks while stopping future work, and `ohara delete <id>`
  records an idempotent knowledge-first deletion intent.
- [x] `ohara er merge` replays recorded Ladybug folds, resolves pending review
  candidates by mention degree/age, remaps aliases, and records `entity_merges`.
- [x] `ohara metrics` reports document milestones, queue state, retained audit
  outcomes, pending ER reviews, recrawl backlog, and raw-payload usage in text
  or JSON form while holding the runtime lock; its LLM section is derived from
  the durable per-attempt Usage Ledger.
- [x] LLM calls record provider, model, outcome, token counts, and configured cost
  estimates in SQLite, including failed attempts; synthesis can use a configured
  quality-fallback model after provider or grounding failure.
- [x] `ohara gc` records zero-mention entity candidates, honors the configured grace
  period, rechecks graph relationships, and removes unmerged entity nodes,
  vectors, aliases, and registry rows idempotently.
- [x] Worker execution, provider assembly, extraction validation, and cross-store
  recovery each have a focused module; the knowledge-port contract suite covers
  vectors, graph links, folds, traversal, and deletion semantics.
- [x] The engine-owned fetch ladder now has deterministic leg selection and
  escalation on anti-bot or JavaScript-required outcomes; the default runtime
  wires plain HTTP plus browser-profile impersonation, and can add the
  executable-backed Obscura provider when configured.
- [x] Site policies now drive conditional re-crawls: due SCRAPE jobs are re-queued,
  validators produce HTTP 304 outcomes, and adaptive intervals are capped by
  fetcher configuration.

## Next priority — provider hardening and embedding migration

Keep each change behind the existing plane ports and run the full verification
gates after every slice.

- [x] The bounded Stage 5.6 synthesis slice is implemented through the existing
  `Llm` port, including the configured quality-fallback flow and per-attempt Usage
  Ledger.
- [x] The ladder composition, deterministic escalation seam, browser-profile
  impersonation leg, and versioned Obscura subprocess adapter are landed.
- [x] The reusable HTTP contract covers success, 304, body limits, not-found
  mapping, and SSRF rejection.
- [ ] Extend the Fetcher contract suite as additional providers are added.

## Embedding migration

- [ ] The current runtime requires one model namespace:
  `knowledge.read_model == knowledge.write_model == embedder.model_id()`.
  Implement and acceptance-test provider dual-write before enabling a split
  read/write migration.

## Stage 5 — Retrieval leftovers
The three-path baseline (BM25 + vector + **graph** + RRF + rerank) and query
entities (typed aliases + `EntityNames` KNN) are done. Remaining:
- [ ] Symspell domain-dictionary correction.
- [ ] Optional `HyDE`.
- [x] Synthesis is landed: top chunks and bounded graph facts are rendered through
  the `Llm` port, structured output is citation-validated, and unavailable or
  ungrounded synthesis falls back to ranked chunks.

## Fetch ladder leg 3 (§15 step 7)
The engine-owned ladder and legs 1–3 are implemented; the default runtime wires
  the third leg only when `fetcher.obscura_command` is configured.
- [x] Leg 2: browser-profile impersonation client — landed; it reuses the hardened
  HTTP transport and declares `stealth` without claiming JavaScript support.
- [x] Leg 3: Obscura subprocess (JS rendering + stealth) — implemented as a
  versioned JSON-line adapter with policy validation and capability/error tests.
- [x] Built-in HTTP and impersonation adapters share a reusable redirect/SSRF and
  policy-limit contract suite.

## `Llm` port leftovers
- [x] Local Ollama is done (OpenAI-compatible completions, JSON-schema structured
  outputs, provider identity, boot health check). The pipeline Usage Ledger records
  all local attempts and the synthesis quality-fallback flow is implemented.
- [ ] Optional cloud Haiku-class provider (opt-in, §12).

## Ops tooling & CLI (§15 step 8)
- [x] The query, backup, document lifecycle, ER merge, and entity GC commands are
  implemented.
- [ ] Stage throughput/latency metrics and a dashboard/export layer (§13).

- [x] `ohara prune` and the read-only `ohara metrics` snapshot are implemented.
- [x] Query/citations/synthesis fallback, backup/restore snapshot safety, document
  lifecycle, and ER merge are landed.

## Deferred small items
- [ ] ER threshold measurement: the `[er]` and `[retrieval]` similarity floors are
  conservative defaults; the golden set needs entity-aware queries (and a
  graph-path rerank-delta metric) before they are measured, not guessed.
