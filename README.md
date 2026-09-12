# Ohara

Ohara builds a private, local knowledge base from documents. It cleans content,
turns it into searchable text and relationships, and answers questions with
citations. It runs as one Rust process with local storage and optional local
Ollama language models.

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
- A local frontend for overview, documents, queries, entity review, and
  operations; the live read-only workflow now covers overview, documents,
  query, and entity reviews, while mutation endpoints are still being added.

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

The default build needs OpenSSL development libraries, CMake, and a C++
toolchain. On Ubuntu or Debian:

```text
sudo apt install libssl-dev cmake g++
```

If system package installation is unavailable, point the build at an OpenSSL
development prefix instead. The `RUSTDOCFLAGS` entry keeps `make verify`
working for doctests as well as normal binaries:

```text
OPENSSL_DIR=/path/to/openssl \
RUSTFLAGS="-L native=/path/to/openssl/lib" \
RUSTDOCFLAGS="-C link-arg=-L/path/to/openssl/lib" \
make verify
```

The Ladybug dependency builds native code on its first build.
The pinned embedding model downloads on first use into local runtime storage.
Afterward it can be used offline.

## Local frontend

Run the Rust API and frontend in separate terminals:

```text
make backend
make frontend
```

Or start both together:

```text
make dev
```

The API listens on `127.0.0.1:3000` and the frontend on `127.0.0.1:5173`.
The frontend uses its mock data by default; the development launchers select the
Rust-backed HTTP mode. The live API supports health, metrics, overview,
documents, query, and entity-review previews. Lifecycle mutations remain
tracked work.

Both individual launch commands free their configured TCP port first. Override
`API_BIND`, `API_PROXY_TARGET`, or `FRONTEND_PORT` when needed.

## Querying

After documents have been indexed, run a local query:

```text
make run ARGS='query "how does WAL checkpointing work?"'
make run ARGS='query "how does WAL checkpointing work?" --top-k 5'
```

The query combines full-text, vector, and graph signals. It returns a bounded
answer with exact evidence citations when the language model produces grounded
output. Otherwise it returns the ranked evidence so retrieval remains useful.

URL registration is currently available through the library integration rather
than a public CLI command. The ingestion UI and a user-facing URL-add workflow
are planned.

## Operator commands

The command-line operator surface includes:

```text
make run ARGS='metrics'
make run ARGS='metrics --json'
make run ARGS='backup /path/to/backup'
make run ARGS='requeue --doc <document-id>'
make run ARGS='archive <document-id>'
make run ARGS='delete <document-id>'
make run ARGS='er merge'
make run ARGS='gc'
make run ARGS='prune --dry-run'
```

Mutating commands coordinate with the worker through a runtime lock. Backups
are staged and do not overwrite an existing destination.

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
