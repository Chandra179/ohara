# Ohara

Ohara turns a web topic into a private local knowledge base. It fetches pages,
cleans their article text, creates searchable chunks, builds a relationship
graph, and answers questions with local citations.

## Architecture

Ohara is a Rust monorepo with five independent processes:

```text
scraper → cleaning → indexer → graph
                           ↘ retrieval → frontend
```

The processes exchange atomic JSON artifacts in `data/`. Indexer owns Qdrant
vectors, graph owns FalkorDB relationships, and retrieval owns hybrid search
and the browser HTTP interface. The frontend is React + TypeScript + Vite and
runs locally with npm.

The active workspace has no root Rust package. Every Rust package has its own
`Cargo.toml`, `src/`, and Dockerfile. The root `Cargo.toml` is workspace-only;
the legacy root implementation has been removed.

Read [system architecture](docs/ARCHITECTURE.md) and the [architecture index](docs/architecture/README.md)
for the process Interfaces and artifact shapes.

Verification and benchmark commands are implemented in the non-production
`tools` workspace package. It contains five Rust binaries and shared test
support; it is not a sixth deployable Ohara process, and production packages
do not depend on it.

## Requirements

- Rust 1.95.0 through rustup
- Node.js and npm for the frontend
- Docker Compose for Qdrant and FalkorDB
- Ollama with `phi4-mini:latest` for local answer synthesis
- Optional Obscura binary for DuckDuckGo discovery or JavaScript-rendered page
  fetching

## Local development

Start each log stream separately:

```sh
make providers
make scraper
make cleaning
make indexer
make graph
make retrieval
make frontend
```

Or start all processes from one terminal:

```sh
make dev
```

The retrieval HTTP process listens on `127.0.0.1:3000`, the scraper listens on
`127.0.0.1:3010`, and the frontend listens on `127.0.0.1:5173`.

For a deployment that places retrieval and scraper on different hosts, set the
same high-entropy `OHARA_PROCESS_AUTH_TOKEN` for both processes. Retrieval
then sends a bearer token to scraper, which rejects missing or invalid tokens.
Protect that connection with HTTPS or a private network. The token is unset by
default for local development.

### Scraper configuration

All scraper settings are in
[`scraper/config.yaml`](scraper/config.yaml). Edit that file to select
`bing-news`, `google-news`, `brave`, `duckduckgo`, or `rss`, configure locale,
and choose the `http` or `obscura` page fetcher. The process loads the file at
startup, so `make scraper` needs no scraper-specific Makefile variables.

The file supports `${ENV_VAR:-default}` overrides for deployment-specific
values. Keep secrets out of the file; provide the Brave key only when needed:

```sh
OHARA_SCRAPER_BRAVE_API_KEY="$BRAVE_SEARCH_API_KEY" make scraper
```

`OHARA_SCRAPER_CONFIG` can point to another YAML file for isolated tests or a
deployment-specific configuration. Obscura is optional and is not bundled in
the default lightweight scraper image.

Brave Search uses its official API and requires an API key. DuckDuckGo does not
offer an official full web-results API, so Ohara uses Obscura's documented
headless-browser `fetch --eval` interface for that adapter. See the
[scraper module contract](docs/architecture/scraper.md) for the full
configuration reference.

The Compose stack can build and run all Rust processes:

```sh
make docker-up
make docker-logs
make docker-down
```

Qdrant uses host ports `6335` (HTTP) and `6336` (gRPC). FalkorDB uses host
port `6380`. Override ports or resource budgets with environment variables in
the Compose command when another local project already uses those ports.

## Topic and query workflow

1. Open `http://127.0.0.1:5173/`.
2. Enter a topic such as `september 2026 news` and choose the article limit.
3. Retrieval forwards the request to scraper.
4. Scraper writes raw pages and cleaning work.
5. Cleaning, indexer, and graph consume their inboxes.
6. Ask a question from the Query screen after indexing completes.

If a stage rejects an artifact, its input is retained under
`data/dead-letter/<stage>/` and the document catalog records the failure.
Retry a bounded number of failures with `make replay STAGE=cleaning LIMIT=10`
(use `indexer` or `graph` for the other consumers).

`POST /api/query` returns ranked chunks even when Ollama is unavailable. A
grounded answer is returned only when synthesis produces non-empty output;
otherwise the response explicitly reports unavailable or ungrounded state.

Run the deterministic process-boundary harness with local test doubles using:

```sh
make pipeline-fixture
```

The same Rust tooling powers the latency, resource, retrieval-quality, and
Qdrant HNSW commands below. The Makefile keeps the stable command names while
Cargo owns compilation and execution; no Python runtime is required.

It starts the real scraper, cleaning, indexer, and retrieval binaries against
fixture RSS/HTML, Qdrant, and Ollama endpoints. It does not use external
network services or download an embedding model.

Measure deterministic retrieval latency with cold process starts and warm
repeated queries:

```sh
make pipeline-benchmark
```

The benchmark reports p50 and p95 for cold startup-plus-first-query and warm
query latency. It fails when the default limits are exceeded; override them
with `OHARA_BENCHMARK_COLD_P50_MS`, `OHARA_BENCHMARK_COLD_P95_MS`,
`OHARA_BENCHMARK_WARM_P50_MS`, or `OHARA_BENCHMARK_WARM_P95_MS` when a machine
needs an explicitly documented budget.

Measure peak resident memory for every process with a representative corpus:

```sh
make pipeline-resource-benchmark
```

The measurement runs each process in isolation with local test doubles and
samples Linux process high-water RSS. It uses deterministic embeddings by
default; set `OHARA_RESOURCE_BENCHMARK_EMBEDDING_MODE=fastembed` to measure the
cached `bge-small-en-v1.5` model. The model cache defaults to `data/models` and
can be changed with `OHARA_RESOURCE_BENCHMARK_MODEL_CACHE`. Set
`OHARA_RESOURCE_BENCHMARK_OUTPUT=path.json` to save the per-process result for
capacity planning. The model-backed baseline and current Compose limits are
documented in [build and operations](docs/architecture/operations.md).

Run the deterministic golden retrieval evaluation:

```sh
make retrieval-quality
```

It runs the real retrieval process against local Qdrant and Ollama test doubles
and reports recall@k, MRR, and nDCG. The fixture is a ranking and contract
regression gate; production semantic quality should be measured again with a
labeled corpus and the configured embedding model.

Measure the graph entity-resolution threshold against its labeled fixture:

```sh
make entity-resolution-quality
```

The command reports precision, recall, F1, false merges, and missed merges for
each threshold. It selects `0.98` for the current fixture; approximate merges
below that threshold remain unresolved, and the benchmark does not modify
runtime graph data.

Compare Qdrant exact search with HNSW on the same workload:

```sh
make qdrant-hnsw-benchmark
```

This requires the Qdrant provider (`make providers`). It reports recall@1/3/5/10,
index-build time, query p50/p95, and observed Qdrant container memory. HNSW is
recommended only when recall@10 retains at least 98% of exact search and p95
latency improves; otherwise exact search remains the default. Set
`QDRANT_SEARCH_MODE=hnsw` for the local processes only after that decision.
Use `OHARA_HNSW_BENCHMARK_OUTPUT=path.json` to save machine-readable results.

To rebuild the derived Qdrant collection and FalkorDB graph from durable
artifacts, stop the five Rust processes and run:

```sh
make rebuild
```

The command recreates both derived stores, requeues clean artifacts for the
indexer, and requeues indexed artifacts for graph publication. It never removes
raw, clean, indexed, or catalog artifacts.

## Verification

```sh
make verify
cd frontend && npm run lint && npm test -- --run && npm run build && npm run e2e
cd .. && make pipeline-fixture && make pipeline-benchmark
cd .. && make pipeline-resource-benchmark
cd .. && make retrieval-quality
cd .. && make entity-resolution-quality
cd .. && make qdrant-hnsw-benchmark
```

Runtime data under `data/` is local and ignored by git. The bounded replay
command is for failed inbox artifacts; `make rebuild` reconstructs the derived
stores from durable artifacts.
