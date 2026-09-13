# TODO

Prioritized work for the five-process Rust workspace. Completed items describe
the new architecture; unchecked items are intentionally open.

## P0 — correctness and operability

- [x] Split the monolithic Rust crate into `scraper`, `cleaning`, `indexer`,
  `graph`, and `retrieval` packages.
- [x] Give every Rust package its own source tree and Dockerfile.
- [x] Define atomic JSON artifact handoffs and deterministic document/chunk ids.
- [x] Replace the old SQLite/Ladybug runtime with Qdrant, FalkorDB, and the
  shared artifact directory.
- [x] Keep the frontend-facing HTTP interface in retrieval and keep frontend
  execution local with npm.
- [x] Add Compose builds, local commands, and small per-process CPU/RAM limits.
- [x] Keep the Makefile aligned with the five-process workspace and remove
  monolith-only run, worker, and port-management targets.
- [x] Remove the legacy root implementation.
- [x] Remove legacy root migrations and tests.
- [x] Add durable per-stage dead-letter directories for failed artifacts.
- [x] Add an explicit, stage-scoped replay command with a per-run item limit.
- [ ] Add a rebuild command that clears derived Qdrant/FalkorDB data and replays
  clean or indexed artifacts safely.

## P1 — stage contracts and production readiness

- [x] Add contract fixtures for every artifact version and reject incompatible
  versions before processing.
- [x] Add per-document processing state and failure reason to catalog updates.
- [ ] Add input/output counters and latency metrics for each process.
- [ ] Add authenticated process-to-process HTTP when stages are deployed on
  different hosts instead of a shared volume.
- [ ] Add graceful drain behavior so a process stops claiming new inbox items
  before shutdown.
- [ ] Add resource-usage documentation based on measured indexer model memory.

## P2 — retrieval quality

- [ ] Add full-text and graph-path signals to retrieval alongside Qdrant.
- [ ] Replace the graph capitalized-phrase baseline with structured extraction
  and typed entity resolution.
- [ ] Measure ER thresholds on ambiguous and cross-document cases.
- [ ] Benchmark HNSW against exact Qdrant search and gate adoption on recall.
- [ ] Evaluate Symspell and HyDE independently for quality and latency.
- [ ] Expand grounded-answer regression fixtures for malformed citations and
  unavailable Ollama responses.

## P3 — frontend and operations

- [ ] Add stage-level progress and throughput charts after backend metrics exist.
- [ ] Add lifecycle actions only with confirmation, authorization, and
  idempotency contracts.
- [ ] Add accessibility, live-stack, and resource-limit checks to CI.
- [ ] Add a quality lab after retrieval and entity-resolution measurements are
  stable.

## P4 — deployment

- [ ] Add separate production Compose overrides for persistent volumes and
  external provider URLs.
- [ ] Add cloud LLM adapters behind an explicit opt-in process configuration.
- [ ] Add horizontal stage partitioning with a durable queue for multi-host
  deployment.
