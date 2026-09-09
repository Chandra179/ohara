# ohara

**ohara** is an embedded, zero-daemon data pipeline for personal-scale knowledge building: it scrapes the web, cleans and normalizes the text, chunks it semantically, and indexes it into a local knowledge store supporting GraphRAG — vector search, property-graph traversal, and cross-encoder reranking — all in one Rust process. No Postgres, no Redis, no Elasticsearch: SQLite is the control plane, LadybugDB (vectors + graph) is the knowledge plane, and Ollama is an optional user-run LLM endpoint.

> **Status:** the core ingestion, conditional re-crawling, vectorization, graph extraction, three-path retrieval baseline, `ohara query`, staged `ohara backup`, document lifecycle commands, offline ER merge executor, and raw retention pruning are implemented. The stabilization matrix covers entity-aware and multi-hop graph cases, duplicate/deletion cleanup, wrong-language and paywall rejection, and retry/dead-letter recovery. The shipped fetcher is HTTP-only (ladder leg 1); the Obscura subprocess and additional ladder legs are planned. LLM synthesis and metrics remain tracked in [TODO.md](TODO.md). The implementation status and target design are kept in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## How it works

```
 [ Stage 1: Scrape ] ──► raw HTML (gz) ──► SQLite 'SCRAPED'
         │          HTTP fetcher (ladder leg 1; later legs are planned)
         ▼
 [ Stage 2: Clean ] ──► normalized Markdown ──► SQLite 'CLEANED'
         │          readability → html2md → sanitize → hash/dedup → quality gate
         ▼
 [ Stage 3: Chunk & Vectorize ] ──► embeddings ──► LadybugDB vectors   'VECTORIZED'
         │          header-aware split → recursive fallback → breadcrumbs
         ▼
 [ Stage 4: Extract Graph ] ──► triplets ──► LadybugDB property graph
         │          LLM extraction → entity resolution → Chunk-[:MENTIONS]->Entity
         ▼                                                          'INDEXED'
 [ Stage 5: Retrieve (GraphRAG) ]
            query normalize → BM25 + vector KNN + graph hops → RRF fusion
            → cross-encoder rerank (configured candidate pool → configured top-k)
            → ranked chunks; LLM synthesis with citations is planned
```

## Tech stack

| Component | Choice | Role |
| :--- | :--- | :--- |
| Scraper engine | `reqwest` HTTP fetcher | Implemented ladder leg 1 with robots, politeness, redirects, and SSRF protection; impersonation and Obscura are planned behind the same `Fetcher` port |
| Control plane | SQLite (WAL, via `rusqlite`) | Job queue, document state machine, dedup hashes, audit log |
| Cleaning | `readability` + `html2md` | Boilerplate removal, HTML → Markdown |
| Text utilities | `whatlang`, Unicode normalization | Language ID and deterministic surface-form normalization; domain-dictionary correction is planned |
| Knowledge store | [LadybugDB](https://github.com/LadybugDB/ladybug) | Embedded property graph plus exact in-engine cosine KNN; HNSW is a future port implementation |
| Embedder | BAAI `bge-small-en-v1.5` via ONNX (pinned, fp32) | 384-dim local embeddings — behind the `Embedder` port |
| Reranker | ONNX `bge-reranker-base` **int8** plus identity baseline | Local cross-encoder reranking behind the `Reranker` port; the identity implementation is the deterministic fallback |
| LLM | [Ollama](https://ollama.com) (local, pinned: `phi4-mini`) | Graph extraction is implemented; synthesis and cloud providers are planned. Nothing leaves the machine by default |

Every third-party engine sits behind a small trait so it can be swapped without touching pipeline logic — see [Ports & substitution](docs/ARCHITECTURE.md#9-ports--substitution-lsp) in the architecture doc.

## Building

Rust 1.95.0 (edition 2024), selected through the installed stable toolchain in
[`rust-toolchain.toml`](rust-toolchain.toml). Verify it with `rustup`:

```text
rustup show active-toolchain
```

Then use the repository commands:

```text
make verify
make build
```

## Querying

After ingesting and indexing documents, query the local GraphRAG index:

```text
make run ARGS='query "how does WAL checkpointing work?"'
make run ARGS='query "how does WAL checkpointing work?" --top-k 5'
```

The command runs the configured BM25, vector, and graph paths and prints ranked
chunks with their immutable `chunk_id` citations. It does not boot the worker or
call the extraction LLM. The default result count is `[retrieval].top_k`.

Create a consistent snapshot while the worker is stopped or quiesced:

```text
make run ARGS='backup /path/to/ohara-backup'
```

The backup takes an exclusive runtime lock, checkpoints and vacuums SQLite,
copies the Ladybug/data artifacts into a staging directory, writes a manifest,
and renames the completed snapshot into place. Existing destinations are never
overwritten, and destinations inside the live data root are rejected.

Manage document lifecycle state while the worker is stopped or quiesced:

```text
make run ARGS='requeue --doc <document-id>'
make run ARGS='archive <document-id>'
make run ARGS='delete <document-id>'
```

`requeue` resets failed or interrupted jobs with a fresh attempt budget.
`archive` keeps indexed chunks queryable while preventing future work. `delete`
records an idempotent intent; the worker or next boot removes knowledge-plane
data first, then applies the SQLite cascade.

Resolve the offline entity-review queue while the worker is stopped or quiesced:

```text
make run ARGS='er merge'
```

The command replays recorded folds for crash recovery, then processes pending
`er_review` rows. It chooses the entity with the higher mention degree (older
entity, then stable id, on ties), remaps typed aliases, records the
`entity_merges` audit row, and folds the Ladybug graph node.

Prune raw payloads while preserving SQLite metadata, clean text, vectors,
graph data, and citations:

```text
make run ARGS='prune --dry-run'
make run ARGS='prune'
```

Configure `[retention] raw_max_bytes` and/or `raw_max_age_days` to select the
retention policy. Both knobs are disabled unless configured; `--dry-run` shows
the selection without deleting files. Pruning requires the same runtime lock
as the worker and other operator commands.

Two native prerequisites are also required:

- **OpenSSL development libraries** — the `lbug` crate's bundled engine links
  OpenSSL at link time: `sudo apt install libssl-dev` (Ubuntu/Debian). Without
  sudo, symlinking the runtime libs into a scratch dir works too:
  `mkdir -p /tmp/ossl/lib && ln -s /usr/lib/x86_64-linux-gnu/libssl.so.3 /tmp/ossl/lib/libssl.so && ln -s /usr/lib/x86_64-linux-gnu/libcrypto.so.3 /tmp/ossl/lib/libcrypto.so && OPENSSL_DIR=/tmp/ossl cargo build`.
- **CMake + a C++ toolchain** — `lbug` compiles its bundled C++ engine on first
  build (this takes a while and needs ~2 GB of scratch space).

The ONNX Runtime and the pinned embedder model (`bge-small-en-v1.5`, ~130 MB)
are fetched on first use from the network and cached under `data/models/`;
afterwards everything runs offline. Heavy native stacks are feature-gated:
`cargo build --no-default-features` drops `lbug` + the ONNX embedder (for
deployments that inject remote `KnowledgeStore`/`Embedder` providers at boot,
§9).

## Repository layout

```
ohara/
├── Cargo.toml
├── README.md
├── docs/
│   ├── ARCHITECTURE.md        # system design (v2.7) — the source of truth
│   └── CODE_GUIDE.md          # code style, API guidelines, lint policy
├── migrations/                # SQL migrations, versioned with the code
├── data/                      # runtime payloads (gitignored): raw/, clean/
├── tests/
│   ├── integration.rs         # suite mount: each tests/integration/*.rs is one suite
│   ├── ports/                 # contract tests — same suite run against every impl of a port
│   └── integration/           # end-to-end tests via the public library API only
└── src/
    ├── main.rs                # binary crate root — thin shell: parse args → library entry point
    ├── lib.rs                 # library crate root — declares the module tree
    ├── config.rs              # settings load + validation into an immutable struct
    ├── control.rs             # CONTROL PLANE facade (SQLite)
    ├── control/
    │   ├── db.rs              #   connections, WAL pragmas, migrations, the one now()
    │   ├── documents.rs       #   document registry, lifecycle state, deletion intents
    │   ├── entities.rs        #   Stage 4 registry: triplets, entities/aliases, ER review + merge audit
    │   ├── jobs.rs            #   job queue: lease claim, stage chaining, requeue
    │   ├── reconcile.rs       #   §7.3 audit-retention portion of the boot sweep
    │   └── models.rs          #   row types + the §6 state machine's shape knowledge
    ├── engine.rs              # ENGINE PLANE facade — pub trait Fetcher (the port)
    ├── engine/
    │   ├── http.rs            #   implemented ladder leg 1: plain HTTP
    │   └── obscura.rs         #   reserved placeholder for a future ladder leg
    ├── knowledge.rs           # KNOWLEDGE PLANE facade — pub trait KnowledgeStore (the port)
    ├── knowledge/
    │   ├── vectors.rs         #   LadybugStore: vector collections (FLOAT[n] node tables,
    │   │                      #   exact in-engine cosine KNN), delete sweep, graph schema
    │   └── graph.rs           #   openCypher: entity upserts, :MENTIONS edges, the §7.8
    │                          #   fold (one transaction), hop-wise fact traversals
    ├── pipeline.rs            # scheduler: claims jobs and delegates lifecycle work to pipeline modules
    ├── pipeline/
    │   ├── execution.rs      # stage dispatch, panic isolation, classification, and atomic audit transitions
    │   ├── runtime.rs         # default provider assembly and shared boot-time compatibility checks
    │   ├── recovery.rs        # cross-store deletion-intent replay and audit-retention ordering
    │   ├── scrape.rs          # Stage 1
    │   ├── clean.rs           # Stage 2 — declares the Extractor port
    │   ├── chunk.rs           # Stage 3a: pure §8 chunker — header-aware split, atomic
    │   │                      #   tables/code, recursive fallback with overlap, breadcrumbs
    │   ├── embed.rs           # Stage 3b: pub trait Embedder (incl. tokenizer counting) +
    │   │                      #   LocalEmbedder (fastembed bge-small-en-v1.5) + the stage body
    │   │                      #   with the §7 replay/repair and delete-first protocol
    │   ├── extract.rs         # Stage 4 orchestration: triplets → entity resolution → graph
    │   │                      #   links + fact-edge aggregation
    │   ├── extraction_contract.rs # Stage 4 prompt/schema/parser/ontology contract
    │   ├── query.rs           # operator retrieval startup and query error boundary
    │   └── retrieve.rs        # Stage 5: three-path retrieval + rerank + QueryNormalizer port
    ├── llm.rs                 # pub trait Llm + the local Ollama provider (structured
                               #   outputs, usage counters, boot health check)
    ├── ops.rs                 # operator facade: lock, backups, lifecycle, and pruning
    │   ├── entity_merge.rs    # offline ER merge orchestration
    │   └── prune.rs           # raw-retention selection and safe unlinking
    └── text.rs                # pure text functions: language ID, normalization, tokenizer, unicode
```

## What each module does, and why

- **`main.rs` / `lib.rs` (two crates, one package).** The library holds all logic; the binary only parses args and calls the worker or operator query entry point. Everything becomes testable without spawning a CLI, and future standalone tools still fit under `src/bin/`.
- **`config.rs`** — Loads and validates every knob (paths, embedder model + version, per-domain rate limits, LLM keys) into an immutable struct at boot: fail fast at startup, never mid-stage.
- **`control/` — the control plane.** Owns *all* SQLite access: documents, the job queue, and the audit trail. The queue lives inside the SQLite module because claiming a job must be an atomic SQL statement against a single-writer WAL database. One directory owns the schema; schema changes touch one place.
- **`engine/` — the fetch engine.** The current implementation gets raw HTML through the plain HTTP leg and exposes capabilities honestly. Future impersonation and Obscura implementations must stay behind the same `Fetcher` port; downstream stages test against a canned fetcher and never require network access.
- **`knowledge/` — the knowledge plane.** Owns *all* LadybugDB access (vectors + graph). Because SQLite and LadybugDB cannot share a transaction, `pipeline/recovery.rs` owns the cross-store boot ordering while each plane keeps its datastore-specific operations testable.
- **`pipeline.rs` + `pipeline/` — orchestration and stages.** The scheduler claims jobs and `pipeline/execution.rs` centralizes dispatch, panic isolation, retry/dead-letter classification, and atomic audit transitions. Stage-specific context types expose only the ports each stage can use. `pipeline/runtime.rs` assembles default providers, `pipeline/recovery.rs` orders cross-store repair, and `pipeline/extraction_contract.rs` isolates Stage 4's provider-facing contract from graph side effects. Query startup lives in `pipeline/query.rs`; stages take their dependencies as traits, which makes them unit-testable in isolation.
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
4. Chunker + local embedder + vector collections + `chunks_fts` — landed; LadybugDB currently uses exact KNN and keeps HNSW as a future port-compatible swap
5. Retrieval baseline: BM25 (FTS5) + vector + rerank — measured on the golden set
6. Stage 4: triplet extraction, entity resolution, graph path — **landed** (Ollama `Llm` provider, §8 matrix validation, `er_review` for near-ties, capped fact-edge aggregation; Stage 5 query entities + the `:MENTIONS` graph path in three-path fusion, with the hermetic machinery baseline measuring 1.000 recall@20 per path and 0.723 fused MRR)
7. Stabilization: retrieval evaluation and the operator CLI slice; restart, deletion, lease, quality-gate, and retry/dead-letter acceptance is landed. `ohara query` prints ranked chunks with citations, `ohara backup` creates consistent snapshots, `requeue`/`archive`/`delete` manage document lifecycle, `er merge` closes the offline merge queue, and `prune` enforces raw retention; metrics remain
8. Obscura leg + full fetch ladder, LLM synthesis, and cost dashboards

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full design: schema, consistency protocol, stage specs, port contracts, error handling, cost model, and security notes.
