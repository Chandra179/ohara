# ohara

**ohara** is an embedded, zero-daemon data pipeline for personal-scale knowledge building: it scrapes the web, cleans and normalizes the text, chunks it semantically, and indexes it into a local knowledge store supporting GraphRAG — vector search, property-graph traversal, and cross-encoder reranking — all in one Rust process. No Postgres, no Redis, no Elasticsearch: SQLite is the control plane, LadybugDB (vectors + graph) is the knowledge plane, and Ollama is an optional user-run LLM endpoint.

> **Status:** the core ingestion, conditional re-crawling, vectorization, graph extraction, three-path retrieval baseline, citation-preserving LLM synthesis with quality fallback, `ohara query`, staged `ohara backup`, document lifecycle commands, offline ER merge executor, entity GC, raw retention pruning, read-only operator metrics, and the initial loopback UI API slice are implemented. The stabilization matrix covers entity-aware and multi-hop graph cases, duplicate/deletion cleanup, wrong-language and paywall rejection, and retry/dead-letter recovery. The fetch ladder wires plain HTTP and browser-profile impersonation, and can add the optional Obscura subprocess when configured. Cloud providers, stage throughput dashboards, HNSW, Symspell, HyDE, and the remaining UI document/entity/lifecycle APIs remain tracked in `TODO.md`. The implementation status and target design are kept in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

The engine-owned `FetchLadder` provides deterministic selection and escalation; the default runtime wires HTTP leg 1 and browser-profile impersonation leg 2, and adds the Obscura leg 3 when `fetcher.obscura_command` is configured.

## How it works

```
 [ Stage 1: Scrape ] ──► raw HTML (gz) ──► SQLite 'SCRAPED'
         │          FetchLadder → HTTP → impersonation → Obscura (optional)
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
            → ranked chunks → bounded LLM synthesis with chunk_id citations
```

## Tech stack

| Component | Choice | Role |
| :--- | :--- | :--- |
| Scraper engine | `FetchLadder` + `reqwest` HTTP fetchers + optional Obscura adapter | Ladder selection/escalation and legs 1–3 are behind `Fetcher`; built-in policy checks cover robots, politeness, redirects, body limits, and SSRF |
| Control plane | SQLite (WAL, via `rusqlite`) | Job queue, document state machine, dedup hashes, audit log |
| Cleaning | `readability` + `html2md` | Boilerplate removal, HTML → Markdown |
| Text utilities | `whatlang`, Unicode normalization | Language ID and deterministic surface-form normalization; domain-dictionary correction is planned |
| Knowledge store | [LadybugDB](https://github.com/LadybugDB/ladybug) | Embedded property graph plus exact in-engine cosine KNN; HNSW is a future port implementation |
| Embedder | BAAI `bge-small-en-v1.5` via ONNX (pinned, fp32) | 384-dim local embeddings — behind the `Embedder` port |
| Reranker | ONNX `bge-reranker-base` **int8** plus identity baseline | Local cross-encoder reranking behind the `Reranker` port; the identity implementation is the deterministic fallback |
| LLM | [Ollama](https://ollama.com) (local, pinned: `phi4-mini`) | Graph extraction and bounded citation-preserving synthesis with optional fallback; every attempt is recorded in the durable Usage Ledger. Cloud providers remain planned. Nothing leaves the machine by default |

Every third-party engine sits behind a small trait so it can be swapped without touching pipeline logic. See the [architecture component contracts](docs/architecture/pipeline.md) and [knowledge contract](docs/architecture/knowledge-plane.md).

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

## Optional Obscura leg

Set `fetcher.obscura_command` to an executable wrapper to enable JavaScript rendering and stealth:

```toml
[fetcher]
obscura_command = "/usr/local/bin/obscura"
```

The wrapper reads the versioned JSON request on stdin and writes one JSON response on stdout; see Stage 1 in the architecture document for the protocol contract.

## Querying

After ingesting and indexing documents, query the local GraphRAG index:

```text
make run ARGS='query "how does WAL checkpointing work?"'
make run ARGS='query "how does WAL checkpointing work?" --top-k 5'
```

The command runs the configured BM25, vector, and graph paths, then asks the
configured synthesis model for a bounded JSON answer grounded in the top chunks
and labeled graph facts. If the primary provider or grounding fails and
`[llm].fallback_model` is configured, the same bounded request is attempted once
with that model. Successful answers print exact immutable `chunk_id` citations;
otherwise the command prints ranked chunks. It does not boot the worker or run
the extraction health gate. The default result count is `[retrieval].top_k`; tune
`[llm].synthesis_model`, `[llm].fallback_model`,
`[retrieval].synthesis_context_chars`, and `[retrieval].synthesis_max_tokens` in
TOML.

## Local UI API

Run the optional API boundary and frontend in separate terminals:

```text
make backend
make frontend
```

The combined `make dev` launcher is also available when a single terminal is
preferred.

The server binds to `127.0.0.1:3000` by default and currently exposes
`GET /api/health`, `GET /api/metrics`, and `POST /api/query`. The frontend keeps
the typed mock adapter as its default, so unset `VITE_OHARA_API_MODE` for the
offline demo. `make dev` sets HTTP mode automatically. The API is read-only at
this stage; document, entity-review, and lifecycle endpoints remain in
`TODO.md`.

`make backend` and `make frontend` free their respective TCP ports before
starting. The frontend waits for the API to answer health checks and cannot
silently move to a different port. Override `API_BIND`, `API_PROXY_TARGET`, or
`FRONTEND_PORT` when needed. On Linux, when only versioned OpenSSL runtime
libraries are available, it creates a compatibility directory under
`OPENSSL_FALLBACK_DIR` (default `/tmp/ohara-ossl`) and supplies the linker path
automatically. Installing `libssl-dev` remains the preferred system setup.

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

Collect unmerged entities that have stayed unmentioned for the configured grace
period:

```text
make run ARGS='gc'
```

The sweep records zero-mention candidates, rechecks graph relationships before
deletion, removes the Ladybug node and `EntityNames` vector, then removes the
SQLite aliases and registry row. Set `[er].entity_gc_grace_days` to control the
grace period; merge-audit-referenced entities are retained.

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

Inspect the current control-plane and raw-payload metrics:

```text
make run ARGS='metrics'
make run ARGS='metrics --json'
```

Metrics reads document milestones, queue state, retained stage events, pending
ER reviews, recrawl backlog, raw-payload usage, and durable LLM calls/outcomes/
tokens/cost while holding the shared runtime lock. JSON output is suitable for
scripts; it does not mutate stores.

Two native prerequisites are also required:

- **OpenSSL development libraries** — the `lbug` crate's bundled engine links
  OpenSSL at link time: `sudo apt install libssl-dev` (Ubuntu/Debian). Without
  sudo, symlinking the runtime libs into a scratch dir works too:
  `mkdir -p /tmp/ossl/lib && ln -s /usr/lib/x86_64-linux-gnu/libssl.so.3 /tmp/ossl/lib/libssl.so && ln -s /usr/lib/x86_64-linux-gnu/libcrypto.so.3 /tmp/ossl/lib/libcrypto.so && OPENSSL_DIR=/tmp/ossl RUSTFLAGS="-L native=/tmp/ossl/lib" cargo build`.
- **CMake + a C++ toolchain** — `lbug` compiles its bundled C++ engine on first
  build (this takes a while and needs ~2 GB of scratch space).

The ONNX Runtime and the pinned embedder model (`bge-small-en-v1.5`, ~130 MB)
are fetched on first use from the network and cached under `data/models/`;
afterwards everything runs offline. Heavy native stacks are feature-gated:
`cargo build --no-default-features` drops `lbug` + the ONNX embedder (for
deployments that inject remote `KnowledgeStore`/`Embedder` providers at boot,
the injected-provider path).

## Repository layout

```
ohara/
├── Cargo.toml
├── README.md
├── docs/
│   ├── ARCHITECTURE.md        # big-picture system design and source of truth
│   ├── architecture/          # focused component contracts
│   └── CODE_GUIDE.md          # code style, API guidelines, lint policy
├── migrations/                # SQL migrations, versioned with the code
├── data/                      # runtime payloads (gitignored): raw/, clean/
├── tests/
│   ├── integration.rs         # suite mount: each tests/integration/*.rs is one suite
│   ├── ports/                 # contract tests — same suite run against every impl of a port
│   └── integration/           # end-to-end tests via the public library API only
└── src/
    ├── main.rs                # binary crate root — boots the dedicated CLI module
    ├── cli.rs                 # argument parsing, command dispatch, and output
    ├── lib.rs                 # library crate root — declares the module tree
    ├── config.rs              # settings load + validation into an immutable struct
    ├── control.rs             # CONTROL PLANE facade (SQLite)
    ├── control/
    │   ├── db.rs              #   connections, WAL pragmas, migrations, the one now()
    │   ├── documents.rs       #   document registry, lifecycle state, deletion intents
    │   ├── entities.rs        #   Stage 4 registry: triplets, entities/aliases, ER review + merge audit
    │   ├── jobs.rs            #   job queue: lease claim, stage chaining, requeue
    │   ├── metrics.rs         #   durable document, queue, audit, ER, recrawl, and LLM aggregates
    │   ├── reconcile.rs       #   §7.3 audit-retention portion of the boot sweep
    │   └── models.rs          #   row types + the §6 state machine's shape knowledge
    ├── engine.rs              # ENGINE PLANE facade — pub trait Fetcher (the port)
    ├── engine/
    │   ├── http.rs            #   shared hardened HTTP transport
    │   ├── impersonate.rs     #   implemented ladder leg 2: browser profile
    │   ├── ladder.rs          #   deterministic leg selection and escalation
    │   └── obscura.rs         #   optional leg 3: versioned JSON subprocess adapter
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
    │   ├── query.rs            # operator retrieval startup and query error boundary
    │   ├── synthesis.rs        # bounded Stage 5.6 prompt/schema/citation contract + fallback
    │   ├── usage.rs            # Usage Ledger adapter around LLM completion attempts
    │   └── retrieve.rs         # Stage 5: paths, context assembly, rerank + QueryNormalizer port
    ├── llm.rs                 # pub trait Llm + the local Ollama provider (structured
                               #   outputs, provider identity, boot health check)
    ├── server.rs              # optional loopback HTTP API for the local frontend
    ├── ops.rs                 # operator facade: lock, metrics, backups, lifecycle, and pruning
    │   ├── entity_gc.rs       # zero-mention entity collection orchestration
    │   ├── entity_merge.rs    # offline ER merge orchestration
    │   ├── metrics.rs         # read-only control/raw usage snapshot
    │   └── prune.rs           # raw-retention selection and safe unlinking
    └── text.rs                # pure text functions: language ID, normalization, tokenizer, unicode
```

## What each module does, and why

- **`main.rs` / `cli.rs` / `lib.rs` (two crates, one package).** The library holds domain logic; `main.rs` only boots `cli.rs`, which parses args and calls library worker/operator services. Everything becomes testable without spawning a CLI, and future standalone tools still fit under `src/bin/`.
- **`config.rs`** — Loads and validates every knob (paths, embedder model + version, per-domain rate limits, LLM keys) into an immutable struct at boot: fail fast at startup, never mid-stage.
- **`control/` — the control plane.** Owns *all* SQLite access: documents, the job queue, and the audit trail. The queue lives inside the SQLite module because claiming a job must be an atomic SQL statement against a single-writer WAL database. One directory owns the schema; schema changes touch one place.
- **`engine/` — the fetch engine.** `FetchLadder` owns neutral leg selection and deterministic escalation while each provider remains behind `Fetcher`. The default runtime wires plain HTTP and browser-profile impersonation, and adds the optional Obscura subprocess when configured; providers enforce shared SSRF, redirect, robots, body-limit, and politeness policy at their boundary. Reusable contract tests cover the built-in HTTP adapters. Downstream stages test against canned fetchers and never require network access.
- **`knowledge/` — the knowledge plane.** Owns *all* LadybugDB access (vectors + graph). Because SQLite and LadybugDB cannot share a transaction, `pipeline/recovery.rs` owns the cross-store boot ordering while each plane keeps its datastore-specific operations testable.
- **`pipeline.rs` + `pipeline/` — orchestration and stages.** The scheduler claims jobs and `pipeline/execution.rs` centralizes dispatch, panic isolation, retry/dead-letter classification, and atomic audit transitions. Stage-specific context types expose only the ports each stage can use. `pipeline/runtime.rs` assembles default providers, `pipeline/recovery.rs` orders cross-store repair, `pipeline/extraction_contract.rs` isolates Stage 4's provider-facing contract from graph side effects, and `pipeline/synthesis.rs` isolates Stage 5.6's prompt/schema/citation contract. Query startup lives in `pipeline/query.rs`; stages take their dependencies as traits, which makes them unit-testable in isolation.
- **`llm.rs` / `pipeline/usage.rs`** — `llm.rs` owns the provider Interface and response token data; `pipeline/usage.rs` is the single Adapter that records each Completion Attempt and computes configured cost estimates into the control-plane Usage Ledger.
- **`text.rs`** — Pure functions (zero I/O, no async) shared by three stages: the cheapest code to test exhaustively, and it keeps algorithms out of orchestration files.

## Conventions

- **Module system:** pure Rust Book ch07 — `pub mod` planes at the crate root, private leaf modules (`mod jobs;`), selective facade re-exports (`pub use models::{Document, Job};`), absolute `crate::` paths, `super::` only for parent-sibling access. The compiler enforces every plane boundary.
- **Ports, not vendors:** `pipeline/` depends only on traits (`Fetcher`, `KnowledgeStore`, `Embedder`, `Reranker`, `Llm`, `Extractor`, `QueryNormalizer`). Contracts are postconditions ("after `delete_doc`, KNN never returns that doc's chunks"), capabilities are declared honestly (`js_rendering: bool`), and vendor query languages never pass through a port. Substitutability is verified by contract tests in `tests/ports/` run against *every* impl plus fakes.
- **Errors (Rust Book ch09):** `Result` at every boundary — boundary failure is *expected*; `panic!` only for broken internal invariants; domain outcomes (paywalled, duplicate, low quality) are values, not errors. Port errors carry a retry classification that drives the job state machine. The worker loop isolates panics (`catch_unwind`) so one bad document can't kill the process.
- **Determinism:** immutable rows are content-hash keyed (`chunk_id = sha256(doc_id:seq)`); entities carry stable surrogate IDs and merge via an explicit protocol; writes are idempotent upserts; models and migrations are pinned — retries and boot-time reconciliation are always safe.

## Roadmap

1. Scaffold the crate: module tree, config, migrations, worker-loop skeleton
2. Control store: documents + jobs (lease claiming)
3. Fetch ladder legs 1–3 (HTTP, browser-profile impersonation, and optional Obscura) + Stages 1–2
   (clean, dedup, quality gate)
4. Chunker + local embedder + vector collections + `chunks_fts` — landed; LadybugDB currently uses exact KNN and keeps HNSW as a future port-compatible swap
5. Retrieval baseline: BM25 (FTS5) + vector + rerank — measured on the golden set
6. Stage 4: triplet extraction, entity resolution, graph path — **landed** (Ollama `Llm` provider, §8 matrix validation, `er_review` for near-ties, capped fact-edge aggregation; Stage 5 query entities + the `:MENTIONS` graph path in three-path fusion, with the hermetic machinery baseline measuring 1.000 recall@20 per path and 0.723 fused MRR)
7. Stabilization: retrieval evaluation and the operator CLI slice; restart, deletion, lease, quality-gate, and retry/dead-letter acceptance is landed. `ohara query` synthesizes bounded answers with primary/fallback models and citations, `ohara backup` creates consistent snapshots, `requeue`/`archive`/`delete` manage document lifecycle, `er merge` closes the offline merge queue, `gc` collects unmerged zero-mention entities, `prune` enforces raw retention, and `metrics` reports durable control/raw/LLM usage state
8. Optional local UI boundary: initial loopback API slice (`serve`, health, metrics, query) with typed frontend integration; remaining UI endpoints stay tracked separately
9. Remaining feature work: cloud LLM providers, embedding dual-write migration, Symspell/HyDE evaluation, stage throughput/latency metrics, dashboards/export, and HNSW

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the system overview and [docs/architecture/README.md](docs/architecture/README.md) for focused component contracts.
