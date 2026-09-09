# TODO

Work remaining to bring the pipeline to the full GraphRAG architecture described
in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). Build-order references are §15
steps. Anything not listed here is implemented (or a deliberate, documented
non-goal).

## Stabilization before feature expansion
These are the next maintenance priorities before tuning retrieval or adding more
providers. Keep each change behind the existing plane ports and run the full
verification gates after every slice.

- Expand the retrieval evaluation set before changing thresholds: add
  entity-aware, multi-hop, duplicate/deletion, wrong-language, paywall, and
  failure/retry cases; record per-path recall, fused MRR, rerank delta, and
  regression baselines.
- Add operational acceptance coverage: boot/restart reconciliation, replay
  idempotence, deletion cleanup across SQLite/FTS/vector/graph stores, and
  retry/dead-letter behavior.
- Finish the first usable operator slice: query/retrieval from the CLI with
  citations, plus backup, requeue, archive, delete, and basic health/metrics
  output. Keep these as explicit commands rather than leaking store internals.

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
`main.rs` only boots the worker; these subcommands are missing:
- `ohara backup` (quiesce + consistent snapshot, §12)
- `ohara prune` (raw retention budget, §7.9)
- `ohara requeue --doc <id>`
- `ohara er merge` (the §7.8 offline merge executor: alias remap, `entity_merges`
  audit row, Ladybug fold via `fold_entity` — Stage 4 files `er_review`
  candidates; the merge tool resolves them)
- `ohara archive <doc>` / `ohara delete <doc>`
- A query / REPL command (retrieval is currently library/tests-only)
- Cost / metrics dashboards (§13)

Implementation order for this section: backup/restore safety, query with
citations, requeue/archive/delete, then ER merge and dashboards.

## Deferred small items
- Entity GC after deletion (§7.6): entities whose `MENTIONS` degree drops to zero
  become GC candidates after a config grace period — no sweeper exists yet.
- ER threshold measurement: the `[er]` and `[retrieval]` similarity floors are
  conservative defaults; the golden set needs entity-aware queries (and a
  graph-path rerank-delta metric) before they are measured, not guessed.
