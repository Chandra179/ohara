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
vectors, graph owns FalkorDB relationships, and retrieval owns the browser HTTP
interface. The frontend is React + TypeScript + Vite and runs locally with npm.

The active workspace has no root Rust package. Every Rust package has its own
`Cargo.toml`, `src/`, and Dockerfile. The root `Cargo.toml` is workspace-only;
the legacy root implementation has been removed.

Read [system architecture](docs/ARCHITECTURE.md) and the [architecture index](docs/architecture/README.md)
for the process Interfaces and artifact shapes.

## Requirements

- Rust 1.95.0 through rustup
- Node.js and npm for the frontend
- Docker Compose for Qdrant and FalkorDB
- Ollama with `phi4-mini:latest` for local answer synthesis

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

It starts the real scraper, cleaning, indexer, and retrieval binaries against
fixture RSS/HTML, Qdrant, and Ollama endpoints. It does not use external
network services or download an embedding model.

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
cd .. && make pipeline-fixture
```

Runtime data under `data/` is local and ignored by git. The bounded replay
command is for failed inbox artifacts; `make rebuild` reconstructs the derived
stores from durable artifacts.
