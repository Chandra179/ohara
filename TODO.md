# TODO

Prioritized work for the five-process Rust workspace. Completed root items are
archived under `docs/archive/`; unchecked items are intentionally open.

## P1 — stage contracts and production readiness

- [x] Add a deterministic pipeline fixture harness for scrape → clean → index
  → query.
- [x] Add cold and warm latency benchmarks with p50 and p95 thresholds.
- [ ] Measure peak RSS for every process with a representative corpus.
- [ ] Add authenticated process-to-process HTTP when stages are deployed on
  different hosts instead of a shared volume.
- [ ] Add graceful drain behavior so a process stops claiming new inbox items
  before shutdown.
- [ ] Add resource-usage documentation based on measured indexer model memory.

## P2 — retrieval quality

- [ ] Add a golden retrieval dataset with recall@k, MRR, and nDCG measurements.
- [ ] Require non-empty grounded answers and valid citations in seeded live
  query tests.
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
