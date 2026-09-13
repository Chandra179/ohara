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

The scraper defaults to Bing News RSS. Edit `scraper/config.yaml` to select
`google-news`, `brave`, `duckduckgo`, or `rss`, or to change the page fetcher.
Brave additionally needs the `OHARA_SCRAPER_BRAVE_API_KEY` environment
override. DuckDuckGo discovery needs an Obscura binary and a configured
`fetch.obscura_binary` path. Set `fetch.kind: obscura` when destination pages
require JavaScript rendering. `search.url` can point the `rss` adapter at a
local fixture or internal RSS gateway. The complete provider contract is
documented in `docs/architecture/scraper.md`.

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

Cleaning, indexer, and graph handle `SIGINT` and `SIGTERM` with a bounded
graceful drain. A worker finishes the artifact already in progress, checks the
shutdown state before claiming another inbox item, and exits. Unstarted inbox
items remain durable for the next process start or an explicit replay.

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

Run `make pipeline-resource-benchmark` to measure peak RSS for scraper,
cleaning, indexer, graph, and retrieval. It runs each process separately with
eight representative documents, local standard-library provider doubles, and
deterministic embeddings. The Linux `/proc/<pid>/status` `VmHWM` value is
sampled while the workload runs. This is a process-memory measurement, not a
container-limit check; model-backed indexer memory should be measured by
setting `OHARA_RESOURCE_BENCHMARK_EMBEDDING_MODE` to the configured mode. The
model cache is taken from `data/models` by default; override it with
`OHARA_RESOURCE_BENCHMARK_MODEL_CACHE=/path/to/models` when needed. Set
`OHARA_RESOURCE_BENCHMARK_OUTPUT=path.json` to persist the result.

Two model-backed runs measured on 2026-09-13 produced this baseline range:

| Process | Workload | Observed peak RSS |
| --- | --- | ---: |
| scraper | topic scrape and raw publication | 11.2–11.3 MiB |
| cleaning | representative raw corpus extraction | 12.4–12.6 MiB |
| indexer | chunking and `bge-small-en-v1.5` embedding | 322.6–323.9 MiB |
| graph | representative indexed corpus publication | 6.3 MiB |
| retrieval | ranked query, embedding, and synthesis | 213.4–214.8 MiB |

This baseline used eight representative documents, the local cached model, and
the standard-library provider doubles. It is a sizing reference, not a
production capacity guarantee. The current Compose limits remain 1 GiB for
the indexer and 512 MiB for retrieval, leaving room for larger batches,
allocator variance, and provider-client buffers. Repeat the benchmark on the
deployment host before tightening either limit.

Run `make retrieval-quality` to evaluate the real retrieval HTTP process against
the versioned golden fixture. The command reports macro recall@1/3/5, MRR, and
nDCG@1/3/5, and fails if any configured minimum is not met. It uses local
standard-library provider doubles and deterministic embeddings, so it is safe
for CI and does not measure production semantic quality.

The app containers use the current host UID/GID when started through
`make docker-up`, so their bind-mounted artifacts remain writable. Direct
Compose users can set `OHARA_CONTAINER_USER=uid:gid` for their account.

For a deployment where retrieval and scraper communicate across hosts, set the
same high-entropy `OHARA_PROCESS_AUTH_TOKEN` on both processes. Retrieval sends
the token as a bearer credential and scraper rejects missing or mismatched
credentials with HTTP `401`; `/health` stays public for readiness probes. Put
the HTTP seam behind TLS or an equivalent private network because bearer
tokens must not cross an untrusted plaintext network. The local Makefile and
Compose defaults leave the token unset for loopback development.

Starting resource budgets are defined in Compose, not Dockerfiles: scraper and
cleaning 256 MB, graph 256 MB, retrieval 512 MB, and indexer 1 GB. Qdrant uses
2 GB and FalkorDB 512 MB. These are initial limits; embedding memory and query
latency should be measured before tightening them.

Required checks are `cargo fmt --check`, `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo test --workspace`, and `cargo doc
--workspace --no-deps`. Frontend checks remain npm lint, unit tests, build, and
browser tests.
