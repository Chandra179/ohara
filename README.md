# ohara

**ohara** is an embedded, zero-daemon data pipeline for personal-scale knowledge building: it scrapes the web, cleans and normalizes the text, chunks it semantically, and indexes it into a local knowledge store supporting GraphRAG — vector search, property-graph traversal, and cross-encoder reranking — all in one Rust process. No Postgres, no Redis, no Elasticsearch: SQLite as the control plane, LadybugDB (vectors + graph) as the knowledge plane, and an external fetcher engine as the only moving part.

> **Status:** build order §15 in progress — **Phases 1–3 landed**: crate scaffold; the **control store** (documents, §6 lease queue with transactional stage chaining, boot reconciliation); and **§15 step 3** — fetch ladder leg 1 (plain HTTP with §12 SSRF guard, robots.txt, politeness), the `sites` policy table, URL normalization, and working **Stages 1–2** (scrape → readability clean → dedup → quality + language gate). Stages 3–4 remain honest stubs. The full system design lives in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## How it works

```
 [ Stage 1: Scrape ] ──► raw HTML (gz) ──► SQLite 'SCRAPED'
         │          Fetcher ladder: plain HTTP → impersonation → Obscura
         ▼
 [ Stage 2: Clean ] ──► normalized Markdown ──► SQLite 'CLEANED'
         │          readability → html2md → sanitize → hash/dedup → quality gate
         ▼
 [ Stage 3: Chunk & Vectorize ] ──► embeddings ──► LadybugDB HNSW   'VECTORIZED'
         │          header-aware split → recursive fallback → breadcrumbs
         ▼
 [ Stage 4: Extract Graph ] ──► triplets ──► LadybugDB property graph
         │          LLM extraction → entity resolution → Chunk-[:MENTIONS]->Entity
         ▼                                                          'INDEXED'
 [ Stage 5: Retrieve (GraphRAG) ]
            query normalize → BM25 + vector KNN + graph hops → RRF fusion
            → cross-encoder rerank (top-50 → top-5) → LLM synthesis with citations
```

## Tech stack

| Component | Choice | Role |
| :--- | :--- | :--- |
| Scraper engine | [Obscura](https://github.com/h4ckf0r0day/obscura) (Rust) | JS rendering + anti-bot fetching — third leg of the fetch ladder, behind the `Fetcher` port |
| Control plane | SQLite (WAL, via `rusqlite`) | Job queue, document state machine, dedup hashes, audit log |
| Cleaning | `readability` + `html2md` | Boilerplate removal, HTML → Markdown |
| Text utilities | `whatlang`, `symspell` | Language ID, algorithmic typo correction |
| Knowledge store | [LadybugDB](https://github.com/LadybugDB/ladybug) | Embedded property graph (openCypher) + native HNSW vector index — behind the `KnowledgeStore` port |
| Embedder | BAAI `bge-small-en-v1.5` via ONNX (pinned, fp32) | 384-dim local embeddings — behind the `Embedder` port |
| Reranker | FlashRank (default), ONNX bge-reranker **int8** (optional) | Local cross-encoder reranking — behind the `Reranker` port |
| LLM | [Ollama](https://ollama.com) (local, GPU; pinned: `phi4-mini`) — cloud Haiku opt-in | Extraction + synthesis — behind the `Llm` port; nothing leaves the machine by default |

Every third-party engine sits behind a small trait so it can be swapped without touching pipeline logic — see [Ports & substitution](docs/ARCHITECTURE.md#9-ports--substitution-lsp) in the architecture doc.

## Repository layout

```
ohara/
├── Cargo.toml
├── README.md
├── docs/
│   ├── ARCHITECTURE.md        # system design (v2.1) — the source of truth
│   └── CODE_GUIDE.md          # code style, API guidelines, lint policy
├── migrations/                # SQL migrations, versioned with the code
├── data/                      # runtime payloads (gitignored): raw/, clean/
├── tests/
│   ├── integration.rs         # suite mount: each tests/integration/*.rs is one suite
│   ├── ports/                 # contract tests — same suite run against every impl of a port
│   └── integration/           # end-to-end tests via the public library API only
└── src/
    ├── main.rs                # binary crate root — thin shell: parse args → ohara::run()
    ├── lib.rs                 # library crate root — declares the module tree
    ├── config.rs              # settings load + validation into an immutable struct
    ├── control.rs             # CONTROL PLANE facade (SQLite) + boot reconciliation sweep
    ├── control/
    │   ├── db.rs              #   connections, WAL pragmas, migrations, the one now()
    │   ├── documents.rs       #   document registry, enqueue + dedup, deletion intents
    │   ├── jobs.rs            #   job queue: lease claim, stage chaining, requeue
    │   ├── reconcile.rs       #   §7.3 sweep: interrupted deletions, audit retention
    │   └── models.rs          #   row types + the §6 state machine's shape knowledge
    ├── engine.rs              # ENGINE PLANE facade — pub trait Fetcher (the port)
    ├── engine/
    │   ├── http.rs            #   ladder leg 1–2: plain HTTP / impersonation
    │   └── obscura.rs         #   ladder leg 3: Obscura process, versioned JSON protocol
    ├── knowledge.rs           # KNOWLEDGE PLANE facade — pub trait KnowledgeStore (the port)
    ├── knowledge/
    │   ├── vectors.rs         #   HNSW upsert / KNN / delete
    │   ├── graph.rs           #   openCypher: MERGE entities, :MENTIONS edges, traversals
    │   └── reconcile.rs       #   boot-time sweep: SQLite intent vs. what actually landed
    ├── pipeline.rs            # worker loop: claim jobs, dispatch stages, retries, audit
    ├── pipeline/
    │   ├── scrape.rs          # Stage 1
    │   ├── clean.rs           # Stage 2 — declares the Extractor port
    │   ├── chunk.rs           # Stage 3a: two-layer chunking + breadcrumbs
    │   ├── embed.rs           # Stage 3b: pub trait Embedder + local ONNX impl
    │   ├── extract.rs         # Stage 4: triplets, entity resolution, cross-linking
    │   └── retrieve.rs        # Stage 5: three-path retrieval + rerank + QueryNormalizer port
    ├── llm.rs                 # pub trait Llm — cloud/local clients, retry, cost counters
    └── text.rs                # pure text functions: langid, symspell, tokenizer, unicode
```

## What each module does, and why

- **`main.rs` / `lib.rs` (two crates, one package).** The library holds all logic; the binary only parses args and calls `ohara::run()`. Everything becomes testable without spawning a CLI, and future entry points (standalone worker, re-embed tool) come free under `src/bin/`.
- **`config.rs`** — Loads and validates every knob (paths, embedder model + version, per-domain rate limits, LLM keys) into an immutable struct at boot: fail fast at startup, never mid-stage.
- **`control/` — the control plane.** Owns *all* SQLite access: documents, the job queue, and the audit trail. The queue lives inside the SQLite module because claiming a job must be an atomic SQL statement against a single-writer WAL database. One directory owns the schema; schema changes touch one place.
- **`engine/` — the fetch engine.** Gets raw HTML via a *degradation ladder* (plain HTTP → impersonated client → Obscura). Obscura is young, so only one file knows its CLI protocol, and every downstream stage tests against a canned-HTML `Fetcher` — no unit test ever touches the network.
- **`knowledge/` — the knowledge plane.** Owns *all* LadybugDB access (vectors + graph) plus `reconcile.rs`, because SQLite and LadybugDB cannot share a transaction: consistency is an explicit protocol (deterministic IDs, idempotent upserts, boot-time reconciliation), and that protocol needs a home it can be tested in.
- **`pipeline.rs` + `pipeline/` — orchestration and stages.** The worker loop claims jobs and dispatches; each stage is one file and one state-machine transition, so a change to chunking never touches graph extraction and every status is greppable. Stages take their dependencies as traits, which makes them unit-testable in isolation.
- **`llm.rs`** — The single LLM port: provider impls, retry/backoff, token/cost counters, prompt templates. Three stages call LLMs; without one port, cost accounting and the data-governance decision (what content leaves the machine) scatter everywhere.
- **`text.rs`** — Pure functions (zero I/O, no async) shared by three stages: the cheapest code to test exhaustively, and it keeps algorithms out of orchestration files.

## Conventions

- **Module system:** pure Rust Book ch07 — `pub mod` planes at the crate root, private leaf modules (`mod jobs;`), selective facade re-exports (`pub use models::{Document, Job};`), absolute `crate::` paths, `super::` only for parent-sibling access. The compiler enforces every plane boundary.
- **Ports, not vendors:** `pipeline/` depends only on traits (`Fetcher`, `KnowledgeStore`, `Embedder`, `Reranker`, `Llm`, `Extractor`, `QueryNormalizer`). Contracts are postconditions ("after `delete_doc`, KNN never returns that doc's chunks"), capabilities are declared honestly (`js_rendering: bool`), and vendor query languages never pass through a port. Substitutability is verified by contract tests in `tests/ports/` run against *every* impl plus fakes.
- **Errors (Rust Book ch09):** `Result` at every boundary — boundary failure is *expected*; `panic!` only for broken internal invariants; domain outcomes (paywalled, duplicate, low quality) are values, not errors. Port errors carry a retry classification that drives the job state machine. The worker loop isolates panics (`catch_unwind`) so one bad document can't kill the process.
- **Determinism:** immutable rows are content-hash keyed (`chunk_id = sha256(doc_id:seq)`); entities carry stable surrogate IDs and merge via an explicit protocol; writes are idempotent upserts; models and migrations are pinned — retries and boot-time reconciliation are always safe.

## Roadmap

1. Scaffold the crate: module tree, config, migrations, worker-loop skeleton
2. Control store: documents + jobs (lease claiming)
3. Fetch ladder leg 1 (HTTP) + Stages 1–2 (clean, dedup, quality gate)
4. Chunker + local embedder + HNSW upsert
5. Retrieval baseline: BM25 (FTS5) + vector + rerank — *no graph yet* — measured on the golden set
6. Stage 4: triplet extraction, entity resolution, graph path
7. Obscura leg + full ladder
8. Eval harness expansion + cost dashboards

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full design: schema, consistency protocol, stage specs, port contracts, error handling, cost model, and security notes.
