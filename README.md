# Ohara

Ohara builds a private, local knowledge base from documents. It cleans content,
turns it into searchable text and relationships, and answers questions with
citations. The ingestion worker and optional local HTTP API run as separate Rust
processes using SQLite, Qdrant, FalkorDB, and optional local Ollama models.

## What it provides

- Web-content fetching with robots, SSRF, redirect, size, and politeness checks.
- Clean Markdown extraction with language, quality, paywall, and duplicate
  detection.
- Token-aware chunking and local vector embeddings.
- Full-text, vector, and graph retrieval combined into one ranked result.
- Typed entity resolution and auditable offline entity merging.
- Citation-preserving answers with a bounded local-language-model fallback.
- Crash-safe jobs, retries, recovery, backups, lifecycle operations, retention,
  entity cleanup, and durable usage metrics.
- A local frontend for topic discovery, overview, documents, queries, entity
  review, and operations. Topic results can be normalized, deduplicated, and
  queued for the worker from the Overview screen.

For a plain-language explanation of the product and its algorithms, read the
[Ohara overview](docs/OVERVIEW.md).

## How it works

```text
document
   │
   ▼
fetch → clean → chunk → embed → extract entities and facts
                                      │
                                      ▼
                         lexical + vector + graph retrieval
                                      │
                                      ▼
                           ranked evidence and citations
```

The durable control store tracks documents, jobs, audit events, and usage. A
separate knowledge index stores vectors and graph relationships and can be
rebuilt from durable data.

## Quick start

Ohara uses Rust 1.95.0. Check the active toolchain:

```text
rustup show active-toolchain
```

Build and verify the project:

```text
make build
make verify
```

The pinned embedding model downloads on first use into local runtime storage.
Afterward it can be used offline. Qdrant and FalkorDB run as local containers.

## Local development

Start the complete local stack with one command:

```text
make dev
```

`make dev` starts Qdrant and FalkorDB with Docker Compose, then starts the Rust
API, ingestion worker, and Vite frontend in one terminal, so all logs are
visible together.
The API listens on `127.0.0.1:3000` and the frontend on `127.0.0.1:5173`.
Press `Ctrl-C` to stop the complete stack; the launcher forwards the signal to
the service processes, waits for graceful shutdown, and stops the Compose
services.

The Compose file uses Qdrant host ports `6335/6336` and FalkorDB host port
`6380` by default. If
another project is already publishing one of those ports, stop that project
first or override the Compose port and matching `[knowledge]` URL in
`ohara.toml`.

For normal use, this is the only launch command you need. Use the separate
targets only when debugging one service or inspecting its logs in a dedicated
terminal.

To run one service in its own terminal, use:

```text
make backend
make worker
make frontend
```

Start `make services` once before using the focused targets. `make frontend`
expects the API to already be running. `make worker` consumes queued documents
and runs the fetch, clean, chunk, embedding, and graph stages; the API does not
supervise it. These Make targets select the Rust-backed HTTP mode. Pass the
same configuration to both backend and worker, for example:

```text
make backend CONFIG_ARGS='--config /path/to/ohara.toml'
make worker CONFIG_ARGS='--config /path/to/ohara.toml'
```

Use `make services` only when you need to manage Qdrant and FalkorDB without
starting Ohara. `make services-down` stops them and `make services-logs` follows
their logs.

The live API supports health, metrics, overview, topic discovery/queueing, query,
documents, and entity-review previews. Lifecycle mutations remain tracked work.

Knowledge data is derived. If a Qdrant or FalkorDB volume is lost, recreate the
services and re-run vectorization/extraction from the durable control data.

The backend and frontend launch commands free their configured TCP port first. Override
`API_BIND`, `API_PROXY_TARGET`, or `FRONTEND_PORT` when needed. The internal port
cleanup and readiness helpers are used by these launchers and are not needed
during normal development.

## Querying

After documents have been indexed, run a local query:

```text
make run ARGS='query "how does WAL checkpointing work?"'
make run ARGS='query "how does WAL checkpointing work?" --top-k 5'
```

The query combines full-text, vector, and graph signals. It returns a bounded
answer with exact evidence citations when the language model produces grounded
output. Otherwise it returns the ranked evidence so retrieval remains useful.

From the Overview screen, enter a topic such as `september 2026 news` and choose
the maximum number of articles to discover (1–10, default 5). Ohara normalizes
and deduplicates the results before adding new documents to the durable queue.
Start the worker to fetch and index queued documents.

## Operator commands


```text
make services

make backend

make worker

make frontend
```

## Documentation

- [Ohara overview](docs/OVERVIEW.md) — product, architecture, features, and
  algorithms for general readers.
- [System architecture](docs/ARCHITECTURE.md) — ownership, invariants, and
  system-level design.
- [Architecture components](docs/architecture/README.md) — focused technical
  contracts.
- [Code guide](docs/CODE_GUIDE.md) — style, API, lint, and verification policy.
- [Core TODO](TODO.md) and [frontend TODO](frontend/TODO.md) — prioritized open
  work.
