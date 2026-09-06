# TODO

Work remaining to bring the pipeline to the full GraphRAG architecture described
in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). Build-order references are §15
steps. Anything not listed here is implemented (or a deliberate, documented
non-goal).

## Stage 4 — Extract graph (§15 step 6) — *stub, `pipeline/extract.rs`*
The whole stage is unimplemented and currently returns a `Permanent` error.
- Triplet extraction via the `Llm` port (per-chunk LLM → JSON triplets, validated
  against the predicate/type-compatibility matrix, staged into `triplets`).
- Entity resolution: alias/type mapping, within-supertype similarity (normalized
  name + `EntityNames` embeddings), per-document union-find, cross-document
  candidates to `er_review`.
- Graph writes: `MERGE` `(:Chunk)-[:MENTIONS]->(:Entity)` and fact edges
  `(subject, predicate, object)` with `support_count`/`evidence`/`occurrences`.
- Offline merge protocol `ohara er merge` (§7.8).
- Note: the `entities`, `entity_aliases`, `triplets` tables exist as schema; no
  code writes them yet.

## Fetch ladder legs 2–3 (§15 step 7)
Only leg 1 (plain HTTP, `engine/http.rs`) is built.
- Leg 2: impersonation client.
- Leg 3: Obscura subprocess (JS rendering + stealth) — `engine/obscura.rs` is a
  placeholder; implement the versioned JSON subprocess protocol.
- Ladder escalation logic that composes the legs on anti-bot / JS-shell signals.

## `Llm` port impl
`llm.rs` defines the trait + error/usage types only; no provider completes
requests. Needed for Stage 4 extraction and Stage 5 synthesis.
- Local Ollama (default), OpenAI-compatible endpoint, pinned `phi4-mini:latest`.
- Optional cloud Haiku-class provider (opt-in, §12).

## Stage 5 — Retrieval leftovers
Baseline (BM25 + vector + RRF + rerank) is done; the following remain:
- Graph path (`facts_within_hops` + `chunks_for_entities`) and query entities.
- symspell domain-dictionary correction.
- Optional `HyDE`.
- Synthesis via the `Llm` port (§8 Stage 5.6).

## Ops tooling & CLI (§15 step 8)
`main.rs` only boots the worker; these subcommands are missing:
- `ohara backup` (quiesce + consistent snapshot, §12)
- `ohara prune` (raw retention budget, §7.9)
- `ohara requeue --doc <id>`
- `ohara er merge`
- `ohara archive <doc>` / `ohara delete <doc>`
- A query / REPL command (retrieval is currently library/tests-only)
- Cost / metrics dashboards (§13)
