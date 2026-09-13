# TODO

Prioritized backlog for Ohara's five independent Rust processes and local
frontend. A checked item is complete only when its implementation, tests, and
documentation are updated. Completed work remains here for traceability;
details are archived under `docs/archive/`.

## Current state audit — 2026-09-13

- P1 is complete: the process seams, JSON artifact contracts, provider
  adapters, process authentication, graceful drain, metrics, benchmarks, and
  scraper configuration are implemented and verified.
- P2 is partially complete: the golden retrieval evaluation, non-empty answer
  checks, Qdrant retrieval, full-text signal, and graph-path signal exist. The
  graph still uses a capitalized-phrase heuristic, Qdrant still uses exact
  search, and the quality experiments and regression fixtures are incomplete.
- P3 is partially complete: backend metrics and per-process snapshots exist,
  but the frontend does not yet present stage-level progress and throughput
  charts. The frontend CI and live-stack checks are also incomplete.
- P4 remains future deployment work. Local Compose exists, but production
  overrides, cloud model adapters, and multi-host durable work distribution do
  not.

All unchecked items below are still relevant. Wording was updated where an old
prerequisite was already satisfied; no completed implementation needs to be
repeated.

## P1 — stage contracts and production readiness

- [x] Add a deterministic scrape → clean → index → query fixture and verify
  the real process seams with local doubles.
- [x] Add cold and warm latency measurements with p50 and p95 thresholds.
- [x] Measure peak RSS for every process against a representative corpus.
- [x] Add authenticated retrieval-to-scraper HTTP with deployment guidance for
  separate hosts.
- [x] Add graceful drain behavior so workers finish their current artifact and
  stop claiming new work during shutdown.
- [x] Document resource budgets using measured model-backed indexer memory.
- [x] Add configuration-driven Bing News, Google News, Brave Search,
  DuckDuckGo-through-Obscura, and custom RSS adapters.
- [x] Load scraper bind, provider, endpoint, locale, and fetcher settings from
  one validated `scraper/config.yaml` configuration seam.

## P2 — retrieval quality

Execute the open work in this order: answer-contract fixtures, structured
entity extraction, entity-resolution measurement, HNSW benchmarking, then
independent SymSpell and HyDE evaluation. HNSW and answer-fixture work can be
developed independently once their test inputs are fixed.

- [x] Add a versioned golden retrieval dataset with recall@k, MRR, and nDCG
  measurements.
- [x] Require non-empty grounded answers and valid citations in seeded live
  query tests.
- [x] Add Qdrant, full-text, and bounded graph-path signals to retrieval with
  availability metadata and deterministic score ordering.

- [ ] Expand grounded-answer regression fixtures for the complete query
  response contract.

  Exit criteria:

  - Cover empty model output, unavailable Ollama, malformed model responses,
    duplicate citations, unknown chunk ids, and citations with no evidence.
  - Define the expected `availability`, `grounding`, `answer`, and `citations`
    values for every case.
  - Keep invalid or unsupported citations from being reported as grounded.
  - Run the cases through the retrieval Interface and the frontend response
    parser; include them in the normal verification command.

- [ ] Replace the graph capitalized-phrase baseline with structured extraction
  and typed entity resolution.

  Exit criteria:

  - Define the supported entity types and the normalized identity rules for
    names, aliases, casing, punctuation, and diacritics.
  - Keep extraction and resolution behind the graph process's internal Seam;
    do not add a Rust dependency between processes or change the indexed
    artifact contract without a versioned contract decision.
  - Preserve idempotent FalkorDB writes and the existing bounded graph-path
    retrieval signal.
  - Add deterministic fixtures for people, organizations, places, events,
    concepts, products, aliases, and text with no entities.
  - Prove that the production path no longer relies on capitalized phrases as
    its only entity detector.

- [ ] Measure entity-resolution thresholds on ambiguous and cross-document
  cases.

  Exit criteria:

  - Create a labeled fixture containing aliases, acronyms, case variants,
    same-name different-type entities, ambiguous names, and cross-document
    mentions.
  - Sweep the merge threshold and report precision, recall, and F1 for each
    threshold, including false merges and missed merges.
  - Select and document the operating threshold and the unresolved-entity
    behavior; do not silently merge below the threshold.
  - Add a repeatable benchmark command and a regression test for the selected
    threshold.

- [ ] Benchmark Qdrant HNSW against exact search and gate adoption on recall.

  Exit criteria:

  - Run both modes against the same representative corpus, vectors, query set,
    and top-k values.
  - Report recall@1/3/5/10, index-build time, query p50/p95 latency, and peak
    memory for each mode.
  - Use exact search as the quality baseline. Adopt HNSW only when recall@10
    is at least 98% of exact search and p95 latency improves on the
    representative corpus; otherwise keep exact search and record why.
  - Make the selected mode explicit in process configuration and verify that
    changing it cannot mix incompatible vector dimensions or collections.

- [ ] Evaluate SymSpell and HyDE independently for quality, latency, and
  resource cost.

  Exit criteria:

  - Evaluate each technique separately against the same labeled queries and
    baseline retrieval configuration; never enable both for the first result.
  - Measure recall@k, MRR, nDCG, query p50/p95 latency, embedding-call count,
    and peak memory.
  - Keep both disabled by default until one demonstrates a documented quality
    gain within the latency and memory budgets.
  - Record the adopt/reject decision and preserve the baseline comparison so
    later changes remain reproducible.

## P3 — frontend and operations

- [ ] Add stage-level progress and throughput charts using the existing metrics
  snapshots.

  Exit criteria:

  - Show queue depth, processed/failed counts, throughput, and p50/p95 latency
    for scraper, cleaning, indexer, graph, and retrieval.
  - Distinguish unavailable, stale, empty, and healthy metrics without
    presenting missing data as zero.
  - Poll at a bounded interval, avoid request-per-keystroke behavior, and keep
    chart rendering usable on narrow screens.
  - Add frontend unit tests, mock-browser coverage, and a live-stack smoke
    check for the metrics response shape.

- [ ] Define lifecycle-action contracts before implementing lifecycle actions.

  Exit criteria:

  - Enumerate the allowed actions, such as replay, rebuild, retry, or stop;
    each action must have a single owner and an explicit safety scope.
  - Define authorization, confirmation, idempotency key, conflict behavior,
    audit logging, and success/failure response semantics.
  - Document which actions are intentionally unavailable in local-only mode.

- [ ] Implement only the approved lifecycle actions with confirmation,
  authorization, and idempotency enforcement.

  Depends on: the lifecycle-action contract above.

  Exit criteria:

  - Repeated requests with the same idempotency key have one effect.
  - Unauthorized, conflicting, and partially failed requests return explicit
    results and do not corrupt artifacts or derived stores.
  - Add process, retrieval, and frontend tests for success, rejection, retry,
    and restart scenarios.

- [ ] Add Rust and frontend quality gates, live-stack checks, and resource-limit
  checks to CI.

  Exit criteria:

  - On pull requests and pushes, run Rust formatting, Clippy with warnings
    denied, workspace tests, and documentation generation.
  - Run frontend lint, unit tests, build, and the deterministic mock-browser
    suite.
  - Validate Compose configuration and declared CPU/RAM limits without
    requiring production credentials.
  - Keep live-provider and live-browser checks opt-in, clearly labeled, and
    separate from the deterministic required gate.

- [ ] Add a quality lab after retrieval and entity-resolution measurements are
  stable.

  Depends on: P2 structured entity resolution, ER threshold measurement, and
  the grounded-answer regression fixtures.

  Exit criteria:

  - Provide one reproducible command to compare retrieval configurations over
    a versioned labeled corpus.
  - Show ranking metrics, grounded-answer validity, citation validity, latency,
    and resource measurements together.
  - Store machine-readable results with the dataset and configuration version;
    do not make the lab a runtime dependency of retrieval.

## P4 — deployment

- [ ] Add production Compose overrides for persistent volumes, external
  provider URLs, secrets, health checks, and restart policy.

  Exit criteria:

  - Keep local development defaults unchanged and place production-only values
    in an explicit override or deployment configuration.
  - Use persistent volumes for Qdrant, FalkorDB, artifacts, and model caches;
    do not mount the source tree in production.
  - Validate the merged Compose configuration and document backup, restore,
    rebuild, and upgrade procedures.

- [ ] Add cloud LLM adapters behind an explicit opt-in process configuration.

  Exit criteria:

  - Keep Ollama as the default local Adapter and keep provider credentials out
    of artifacts, logs, and frontend responses.
  - Define timeout, retry, rate-limit, privacy, and failure semantics for a
    cloud Adapter before enabling it.
  - Verify that switching adapters does not alter the grounded-answer and
    citation contract.

- [ ] Add horizontal stage partitioning with a durable queue for multi-host
  deployment.

  Exit criteria:

  - Record the queue technology and delivery guarantees before implementation.
  - Define leases, acknowledgements, retry/dead-letter behavior, ordering,
    replay, authentication, and idempotency for every artifact handoff.
  - Demonstrate safe worker replacement and recovery from a host failure.
  - Keep the shared-directory implementation available for local development
    until the durable queue path meets the same artifact contract tests.
