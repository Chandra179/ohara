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
- `ohara query` prints ranked chunks with immutable citations, and `ohara backup`
  writes a staged, non-overwriting snapshot behind the worker runtime lock.

## Next priority — operator slice

Keep each change behind the existing plane ports and run the full verification
gates after every slice.

- Add document lifecycle operations next: `requeue`, `archive`, and `delete`,
  keeping deletion knowledge-first and exposing only control-plane facade types.

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
- Synthesis via the `Llm` port (§8 Stage 5.6): context = top chunks + graph
  facts rendered as a labeled fact list; citations = `chunk_id`s.

## Fetch ladder legs 2–3 (§15 step 7)
Only leg 1 (plain HTTP, `engine/http.rs`) is built.
- Leg 2: impersonation client.
- Leg 3: Obscura subprocess (JS rendering + stealth) — `engine/obscura.rs` is a
  placeholder; implement the versioned JSON subprocess protocol.
- Ladder escalation logic that composes the legs on anti-bot / JS-shell signals.

## `Llm` port leftovers
Local Ollama is done (OpenAI-compatible completions, JSON-schema structured
outputs, usage counters, boot health check). Remaining:
- Optional cloud Haiku-class provider (opt-in, §12).
- The §11.2 quality-fallback model flow (`llm.fallback_model` is config-only).

## Ops tooling & CLI (§15 step 8)
The query and backup commands are implemented; these operator commands are missing:
- `ohara prune` (raw retention budget, §7.9)
- `ohara requeue --doc <id>`
- `ohara er merge` (the §7.8 offline merge executor: alias remap, `entity_merges`
  audit row, Ladybug fold via `fold_entity` — Stage 4 files `er_review`
  candidates; the merge tool resolves them)
- `ohara archive <doc>` / `ohara delete <doc>`
- Cost / metrics dashboards (§13)

Implementation order for this section: requeue/archive/delete, then ER merge and
dashboards. Query/citations and backup/restore snapshot safety are landed.

## Deferred small items
- Entity GC after deletion (§7.6): entities whose `MENTIONS` degree drops to zero
  become GC candidates after a config grace period — no sweeper exists yet.
- ER threshold measurement: the `[er]` and `[retrieval]` similarity floors are
  conservative defaults; the golden set needs entity-aware queries (and a
  graph-path rerank-delta metric) before they are measured, not guessed.
