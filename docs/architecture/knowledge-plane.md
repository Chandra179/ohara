# Knowledge plane

The knowledge plane owns the two external local services used for derived
knowledge:

- Qdrant stores model-scoped chunk and entity-name vectors.
- FalkorDB stores entities, chunk mentions, and aggregated fact edges.

SQLite remains the source of truth for documents, chunks, triplet evidence,
and entity identity. Knowledge data can be rebuilt from those records.

## Contracts

All operations are behind the `KnowledgeStore` port. Vector operations preserve
model isolation, deterministic ids, cosine scores, and delete-to-search
postconditions. Graph operations are idempotent: entities and mentions use
`MERGE` semantics, fact edges aggregate support and bounded evidence, and entity
folds rewire relationships before removing the loser.

The worker creates a writable adapter. API/query processes create read-only
adapters. This is an application ownership rule; service persistence and
concurrency are handled by Qdrant and FalkorDB themselves, not by shared local
files or process locks.

## Service lifecycle

`make dev` starts the services before the Ohara processes. Qdrant listens on
port 6335 and FalkorDB listens on port 6380 by default. Configuration can
override both URLs. Use `make services` when managing only the knowledge
services. Readiness probes both services and reports one actionable knowledge
diagnostic when either is unavailable.

The Compose file owns both services. A Qdrant or Redis-compatible service from
another project must be stopped before reusing these default host ports, or the
Compose port and matching Ohara URL must be overridden together.

The Compose volumes are the knowledge indexes. Back them up with the services'
own snapshot procedures; the Ohara SQLite backup covers control data and local
payloads, not live external-service files.

## Recovery

There is no local knowledge WAL parser or native-store recovery path. If an
index is lost, recreate the services and re-run vectorization/extraction from
the durable control data. Cross-store deletion and retry ordering remains in
the pipeline recovery protocol.
