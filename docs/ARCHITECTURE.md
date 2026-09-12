# Ohara — System Architecture

This is the big-picture design for Ohara. It defines ownership, boundaries,
data flow, and invariants. Detailed component contracts are in the
[component documentation](architecture/README.md); implementation file names
do not belong in this overview.

General readers can start with the [Ohara overview](OVERVIEW.md), which explains
the product, features, and algorithms in plain language.

## 1. Overview

Ohara is an embedded, zero-daemon knowledge pipeline for personal-scale corpora.
It turns web content into clean text, searchable chunks, graph facts, and
citation-preserving answers in one local process. SQLite is the durable control
store; the knowledge index is rebuildable; external services are optional.

### 1.1 Planes

| Plane | Owns | Does not own |
| :--- | :--- | :--- |
| Control | durable documents, jobs, audit, identities, and usage | network or graph operations |
| Engine | outbound HTTP adapters, fetching, and fetch policy | document state or knowledge writes |
| Knowledge | vectors and graph data | queue scheduling or SQLite schema |
| Pipeline | stage order, orchestration, and recovery sequencing | vendor-specific storage or transport |

Runtime/operator services compose the planes. The frontend and local HTTP API
are transport surfaces, not a fourth datastore plane.

### 1.2 Principles

1. One owner per datastore.
2. Ports expose behavior and honest capabilities, not vendor mechanisms.
3. Durable state is authoritative; derived knowledge is rebuildable.
4. Hash-based derived identities and idempotent upserts make replay safe.
5. Boundary failures are values classified for retry, dead-letter, or shutdown.
6. Every stage transition and error is auditable.
7. Pinned configuration and model identity make processing reproducible.

### 1.3 Dependency direction

The pipeline depends on plane facades and behavioral ports. Planes do not depend
on pipeline orchestration. Runtime assembly is the composition root. Transport
surfaces call public services and never issue datastore queries directly.

## 2. Tech stack and risk posture

The default local stack is SQLite/WAL, LadybugDB, a local ONNX embedder, an
identity reranker for the baseline query path, and Ollama when language-model
work is enabled. The fetch engine provides plain HTTP and browser-profile legs,
with an optional executable JavaScript provider.

LadybugDB currently provides exact in-engine cosine KNN; it does not provide an
HNSW index. HNSW is a future port-compatible implementation, not a current
runtime switch. Cloud language-model providers are planned and remain opt-in by
design. Heavy native dependencies are feature-gated.

## 3. Data model

The durable control store contains documents, jobs, chunks, full-text search,
triplet evidence, entity identity and merge records, deletion intents, audit
events, language-model usage, and maintenance state. The knowledge store
contains model-scoped vectors, entity-name vectors, mention relationships, and
aggregated fact edges. Raw and clean payloads remain local runtime data.

Immutable derived rows use content-derived identities (`chunk_id` and
`triplet_id`). Documents, jobs, and entities use stable surrogate identities.
Canonical entity name and type are lookup data, not the entity's identity.

## 4. Embedding layer

The pinned local embedder is English-first, 384-dimensional, cosine-based, and
measures chunk budgets in its own tokenizer. The model/variant identity selects
the vector namespace. The current runtime requires the read namespace, write
namespace, and injected embedder identity to match.

Changing models requires a dual-write migration: populate the new namespace,
update each durable row, switch reads atomically, then retire the old namespace.
That migration is not implemented yet.

## 5. Control-plane contract

The control plane owns the durable state machine, queue, migrations, and
operator metadata. Document status records completed milestones; job status
records execution state. Full-text index maintenance is transactional with
chunk writes. The control facade returns domain records and hides database
handles.

See the [control-plane contract](architecture/control-plane.md).

## 6. Job state machine

Each document has at most one job per stage. A claim moves `PENDING` to
`RUNNING` under a lease. Success records the milestone and schedules the next
stage atomically. Transient failures back off and retry; permanent failures
become `DEAD`; fatal failures drain the worker for recovery at the next boot.

Archived documents remain queryable but are excluded from new work. Requeue is
idempotent and resets failed or interrupted work for the selected document.

## 7. Cross-store consistency

SQLite and LadybugDB do not share a transaction. The system therefore uses
intent-before-write, deterministic identities, idempotent upserts, delete-first
re-chunking, and startup reconciliation. Deletion is knowledge-first, followed
by the SQLite cascade. Entity merges are explicit offline operations; triplet
evidence remains unchanged.

See the [pipeline contract](architecture/pipeline.md) and [knowledge-plane
contract](architecture/knowledge-plane.md).

## 8. Stage specifications

1. **Scrape:** fetch labeled content through the engine and store raw payloads.
2. **Clean:** extract primary text, normalize it, and return accepted, duplicate,
   or quality-rejected outcomes.
3. **Chunk and vectorize:** create bounded breadcrumb-aware chunks, persist their
   registry rows, and write model-scoped vectors.
4. **Extract graph:** validate structured evidence, resolve typed entities, stage
   triplets, and write mentions and fact edges.
5. **Retrieve:** combine lexical, vector, and graph paths, rerank candidates, and
   optionally synthesize a cited answer.

The graph stage is optional. Without it, vectorized documents remain searchable
through lexical and vector retrieval.

See the [pipeline contract](architecture/pipeline.md) and [retrieval
contract](architecture/retrieval.md).

## 9. Ports and substitution

Ports are object-safe, `Send + Sync`, and tested by behavioral postconditions.
They cover fetching, knowledge operations, embedding, reranking, language-model
completion, extraction, and query normalization. A provider must map native
failures to the shared error taxonomy and preserve ordering, identity, and
deletion guarantees.

The `Llm` port owns provider semantics and remains independent of networking.
Outbound HTTP implementations for local or cloud providers belong with the
Engine plane, while runtime assembly selects the adapter and the pipeline sees
only the port. Cloud egress remains disabled unless explicitly configured.

## 10. Error handling

Every boundary returns `Result` for infrastructure failure. Domain outcomes
such as duplicate, paywalled, low-quality, and wrong-language content are
explicit values. Port errors carry retry classification. The worker records
error context, isolates stage panics, and keeps invariant-only panics visible.

## 11. Performance and cost model

The system is LLM-dominated. Local embedding and exact KNN are appropriate for
the target personal-scale corpus; reranking is optional and query-time only.
Future HNSW adoption must be measured against exact-KNN recall before it becomes
the default. Stage throughput and latency metrics are planned.

## 12. Security and governance

Scraped content is untrusted data and is never executed. URL scheme, SSRF,
redirect, robots, body-size, and politeness policies protect outbound access.
Cloud language-model egress is disabled by default. Backups require a quiesced
runtime and never copy live store files. The local API binds to loopback until
an authenticated deployment boundary exists.

## 13. Observability

Durable stage events, queue and milestone aggregates, recrawl state, entity
reviews, raw-payload usage, and every language-model attempt form the current
operator view. The API exposes health, metrics, overview, documents, query, and
read-only entity-review previews. Stage throughput/latency dashboards and export
are planned.

## 14. Testing strategy

Unit tests cover pure behavior and boundaries. Port suites run against every
implementation and verify error mapping, collection isolation, replay safety,
and deletion postconditions. Integration suites exercise public APIs with
deterministic providers. Evaluation measures retrieval-path recall, fused
ranking, and reranking change; real-model tests remain separate and ignored.

The required repository gates are in the [code guide](CODE_GUIDE.md) and the
build/runtime details are in [testing and build](architecture/testing-and-build.md).

## 15. Build order and current status

1. Core control, engine, cleaning, chunking, embedding, and vector indexing —
   implemented.
2. Three-path retrieval, graph extraction, entity resolution, and local
   citation-preserving synthesis — implemented; operator query currently uses
   the identity reranker baseline.
3. Recovery, lifecycle/maintenance operators, and evaluation machinery —
   implemented.
4. Local UI boundary — health, metrics, query, overview, document, and
   read-only entity-review transport implemented; lifecycle mutations remain
   open.
5. Next work — worker process lifecycle/readiness, transport hardening,
   lifecycle API contracts, measured ER/retrieval quality, HNSW, throughput metrics,
   embedding migration, and cloud providers.

## Component documentation

The detailed component contracts are kept separately:

| Component | Contract |
| :--- | :--- |
| Control plane | [control-plane.md](architecture/control-plane.md) |
| Fetch engine | [engine.md](architecture/engine.md) |
| Knowledge plane | [knowledge-plane.md](architecture/knowledge-plane.md) |
| Pipeline stages | [pipeline.md](architecture/pipeline.md) |
| Retrieval and evaluation | [retrieval.md](architecture/retrieval.md) |
| Language-model services | [llm.md](architecture/llm.md) |
| Runtime composition | [runtime.md](architecture/runtime.md) |
| Operator services | [operations.md](architecture/operations.md) |
| Frontend and local API | [ui-api.md](architecture/ui-api.md) |
| Testing and build | [testing-and-build.md](architecture/testing-and-build.md) |
