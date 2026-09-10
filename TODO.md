# TODO

Work remaining to bring the pipeline to the full GraphRAG architecture described
in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). Build-order references are §15
steps. Anything not listed here is implemented (or a deliberate, documented
non-goal).

## Completed stabilization coverage

The following safeguards are implemented and covered before feature expansion:

- The retrieval evaluator records BM25, vector, and graph recall@20, fused MRR,
  rerank delta, and named regression floors.
- Acceptance cases cover typed entity retrieval, one- and two-hop graph context,
  duplicate content surviving sibling deletion, and cleanup across FTS, vectors,
  and graph mentions.
- Quality-gate coverage includes wrong-language and paywall rejection without
  chaining VECTORIZE.
- Boot/restart reconciliation, expired-lease replay, retry-to-dead-letter
  classification, and `requeue` recovery are covered by the worker suite.
- The `Embedder` port exposes provider input capacity; worker boot rejects a
  chunk budget that would exceed it, preventing silent provider truncation.
- `ohara query` synthesizes a bounded answer from top chunks and labeled graph
  facts with exact immutable citations, and falls back to ranked chunks when the
  LLM is unavailable or ungrounded. `ohara backup` writes a staged,
  non-overwriting snapshot behind the worker runtime lock.
- `ohara requeue --doc <id>` resets failed/interrupted jobs, `ohara archive <id>`
  preserves queryable chunks while stopping future work, and `ohara delete <id>`
  records an idempotent knowledge-first deletion intent.
- `ohara er merge` replays recorded Ladybug folds, resolves pending review
  candidates by mention degree/age, remaps aliases, and records `entity_merges`.
- `ohara metrics` reports document milestones, queue state, retained audit
  outcomes, pending ER reviews, recrawl backlog, and raw-payload usage in text
  or JSON form while holding the runtime lock.
- Worker execution, provider assembly, extraction validation, and cross-store
  recovery each have a focused module; the knowledge-port contract suite covers
  vectors, graph links, folds, traversal, and deletion semantics.
- The engine-owned fetch ladder now has deterministic leg selection and
  escalation on anti-bot or JavaScript-required outcomes; the default runtime
  wires plain HTTP plus the browser-profile impersonation provider; Obscura
  remains the next unimplemented leg.
- Site policies now drive conditional re-crawls: due SCRAPE jobs are re-queued,
  validators produce HTTP 304 outcomes, and adaptive intervals are capped by
  fetcher configuration.

## Next priority — Obscura fetch leg and provider hardening

Keep each change behind the existing plane ports and run the full verification
gates after every slice.

The bounded Stage 5.6 synthesis slice is implemented through the existing `Llm`
port. The ladder composition, deterministic escalation seam, and browser-profile
impersonation leg are landed. The remaining production-reliability work is the
versioned Obscura subprocess protocol and deeper provider contract coverage while
preserving the `Fetcher` port. Keep each leg independently testable.

## Embedding migration

- The current runtime requires one model namespace:
  `knowledge.read_model == knowledge.write_model == embedder.model_id()`.
  Implement and acceptance-test provider dual-write before enabling a split
  read/write migration.

## Stage 5 — Retrieval leftovers
The three-path baseline (BM25 + vector + **graph** + RRF + rerank) and query
entities (typed aliases + `EntityNames` KNN) are done. Remaining:
- symspell domain-dictionary correction.
- Optional `HyDE`.
- Synthesis is landed: top chunks and bounded graph facts are rendered through
  the `Llm` port, structured output is citation-validated, and unavailable or
  ungrounded synthesis falls back to ranked chunks.

## Fetch ladder leg 3 (§15 step 7)
The engine-owned ladder and legs 1–2 are wired by the default runtime.
- Leg 2: browser-profile impersonation client — landed; it reuses the hardened
  HTTP transport and declares `stealth` without claiming JavaScript support.
- Leg 3: Obscura subprocess (JS rendering + stealth) — `engine/obscura.rs` is a
  placeholder; implement the versioned JSON subprocess protocol.
- Add reusable provider contract tests for redirect/SSRF, policy limits, and
  error mapping before adding the Obscura implementation.

## `Llm` port leftovers
Local Ollama is done (OpenAI-compatible completions, JSON-schema structured
outputs, usage counters, boot health check). Remaining:
- Optional cloud Haiku-class provider (opt-in, §12).
- The §11.2 quality-fallback model flow (`llm.fallback_model` is config-only).

## Ops tooling & CLI (§15 step 8)
The query, backup, document lifecycle, and ER merge commands are implemented;
these operator commands are missing:
- Durable LLM usage persistence and cost / metrics dashboards (§13)

`ohara prune` and the read-only `ohara metrics` snapshot are implemented.
Query/citations/synthesis fallback, backup/restore snapshot safety, document
lifecycle, and ER merge are landed.

## Deferred small items
- Entity GC after deletion (§7.6): entities whose `MENTIONS` degree drops to zero
  become GC candidates after a config grace period — no sweeper exists yet.
- ER threshold measurement: the `[er]` and `[retrieval]` similarity floors are
  conservative defaults; the golden set needs entity-aware queries (and a
  graph-path rerank-delta metric) before they are measured, not guessed.
