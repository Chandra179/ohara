# Build and operations

The root is a Cargo workspace with five packages. Each Rust package owns its
`Cargo.toml`, `src/`, and Dockerfile. The frontend is a local npm application
and has no Dockerfile.

Local development:

```text
make providers
make scraper
make cleaning
make indexer
make graph
make retrieval
make frontend
```

`make providers` starts only Qdrant and FalkorDB. Use `make providers-down` or
`make providers-logs` to manage or inspect those two provider containers. The
per-process targets keep each Rust log stream separate; `make dev` combines all
five Rust processes and the frontend in one terminal.

`make docker-up` builds and runs the Rust packages and databases through Compose;
`make docker-down` stops the entire Compose stack. Qdrant uses host port 6335
and FalkorDB uses 6380 by default.

Failed inbox artifacts are retained under `data/dead-letter/<stage>/`. Retry a
bounded number of items with `make replay STAGE=cleaning LIMIT=10` (or
`indexer`/`graph`). The command only moves files back to that stage's inbox;
the owning process performs the retry and records the next result.

For a full derived-store rebuild, stop the five Rust processes and run
`make rebuild`. The command deletes and recreates the configured Qdrant
collection, deletes the configured FalkorDB graph, then atomically requeues
every clean artifact for the indexer and every indexed artifact for graph
publication. Raw, clean, indexed, and catalog artifacts are preserved. The
processes must be stopped during the command so they cannot consume or replace
the rebuild inbox while it is being populated.

Each process also writes cumulative input, output, failure, and latency
measurements under `data/state/`. Retrieval returns those snapshots from
`GET /api/metrics` under `stages`; a missing snapshot means that process has
not started yet.

Run `make pipeline-fixture` to exercise the real scraper, cleaning, indexer,
and retrieval binaries against deterministic local RSS/HTML, Qdrant, and
Ollama test doubles. The harness uses `OHARA_EMBEDDING_MODE=deterministic` so
it does not download a model and is suitable for CI.

Run `make pipeline-benchmark` to measure retrieval cold and warm latency with
the same local test doubles. Cold latency includes process startup through the
first successful query; warm latency measures repeated queries after startup.
The command gates both p50 and p95 against configurable millisecond limits and
prints the startup and first-query components for diagnosis.

The app containers use the current host UID/GID when started through
`make docker-up`, so their bind-mounted artifacts remain writable. Direct
Compose users can set `OHARA_CONTAINER_USER=uid:gid` for their account.

Starting resource budgets are defined in Compose, not Dockerfiles: scraper and
cleaning 256 MB, graph 256 MB, retrieval 512 MB, and indexer 1 GB. Qdrant uses
2 GB and FalkorDB 512 MB. These are initial limits; embedding memory and query
latency should be measured before tightening them.

Required checks are `cargo fmt --check`, `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo test --workspace`, and `cargo doc
--workspace --no-deps`. Frontend checks remain npm lint, unit tests, build, and
browser tests.
