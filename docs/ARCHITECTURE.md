# Ohara system architecture

Ohara turns web topics into a local, searchable knowledge base. Five
independently runnable Rust processes exchange JSON artifacts through a shared
data directory. The frontend runs locally with npm and talks only to retrieval.

## System flow

```text
frontend → retrieval → scraper → cleaning → indexer → graph
    ▲          │          │          │          │       │
    └──────────┴──────────┴──────────┴──────────┴───────┘
                     shared artifact directory

indexer → Qdrant       graph → FalkorDB       retrieval → Ollama
```

The flow is intentionally stage-oriented. Each process has one deep Interface:
it accepts one kind of input, performs one domain responsibility, and publishes
one kind of output. A process can be deployed, restarted, or replaced without
linking another process as a Rust dependency.

## Process ownership

| Process | Owns | Publishes |
| :--- | :--- | :--- |
| `scraper` | topic discovery, URL normalization, HTTP fetching | raw HTML and cleaning work |
| `cleaning` | main-content extraction and quality normalization | clean Markdown and indexing work |
| `indexer` | canonical chunking, embeddings, Qdrant writes | indexed chunks and graph work |
| `graph` | entity/mention extraction and FalkorDB writes | graph relationships |
| `retrieval` | frontend HTTP, document projections, vector search, synthesis | answers, citations, health, and metrics |

Qdrant and FalkorDB are external derived stores. The shared artifact directory
is the handoff medium and local source for document metadata. There is no
SQLite control plane and no old monolithic runtime in the new architecture.

## Artifact seam

Each handoff is a JSON file written atomically into the next process's inbox:

```text
data/
├── raw/                         # scraper output
├── clean/                       # cleaning output
├── indexed/                     # indexer output and retrieval evidence
├── catalog/                     # document projection for retrieval
├── inbox/cleaning/*.json
├── inbox/indexer/*.json
├── inbox/graph/*.json
└── state/*.heartbeat            # process readiness
```

The JSON shapes are documented in [artifact contracts](architecture/artifacts.md).
They are deliberately process-local types rather than a shared Rust crate, so
teams can evolve implementations independently while keeping the persisted
shape explicit and reviewable.

## Reliability rules

1. A stage writes its output before removing its input; failed inputs move to
   that stage's dead-letter directory for inspection and replay.
2. Document and chunk identities are deterministic hashes, so retries are
   idempotent.
3. Partial files use temporary names and atomic rename; consumers only read
   completed `.json` files.
4. Derived Qdrant and FalkorDB data can be rebuilt from the artifact directory.
5. Provider failures remain visible in process logs and document catalog state.
6. Each process persists input/output/failure counters and latency snapshots
   under `state/`; retrieval exposes them for operations tooling.
7. The frontend-facing process binds to loopback by default.
8. The retrieval-to-scraper process seam supports a shared bearer token through
   `OHARA_PROCESS_AUTH_TOKEN`. Configure the same non-empty token on both
   processes for remote deployment and use HTTPS or a private network.

## Deployment

The root Compose file builds each Rust process from its own Dockerfile, mounts
the shared data directory, starts Qdrant and FalkorDB, and applies a small
per-process CPU/RAM limit. The frontend is intentionally not containerized for
local development. `make dev` runs the five Rust processes locally and the
frontend with npm; the focused Make targets expose each log stream separately.

Detailed process responsibilities and operational contracts are listed in the
[architecture index](architecture/README.md).
