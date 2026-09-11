# Control plane

The control plane owns the durable SQLite database. It is the source of truth
for document identity, processing state, jobs, audit history, entity registry
data, extraction checkpoints, usage records, and operator metadata.

## Durable records

- Documents and per-host fetch policy.
- One job per `(document, stage)` with priority, attempts, backoff, and lease.
- Chunks, full-text search text, triplet evidence, entities, aliases, merge
  audit, and entity-review candidates.
- Deletion intents, stage events, language-model usage, and entity-GC grace
  state.

SQLite connections enable foreign keys and WAL mode. Migrations are versioned.
Audit events intentionally outlive deleted documents; document-owned data is
cascaded in one control transaction.

## Queue semantics

Document status is a completed milestone (`NEW`, `SCRAPED`, `CLEANED`,
`VECTORIZED`, `INDEXED`, `FAILED_QUALITY`, `FAILED`, or `ARCHIVED`). Job status
is execution state (`PENDING`, `RUNNING`, `DONE`, or `DEAD`). A successful stage
advances its milestone and schedules the next stage atomically. Claims use a
lease, expired work is reclaimable, and retry classification drives backoff or
dead-lettering.

Archived documents remain queryable but are excluded from new work. Requeue is
idempotent and resets only the selected document's failed or interrupted jobs.

## Identity and write rules

Chunk identity is `sha256(document_id:sequence)`. Triplet identity is derived
from the chunk and surface-form evidence. Entity identity is a stable surrogate
and canonical name/type is a lookup key. Derived rows use conflict updates;
replacement inserts are forbidden because they churn full-text row mappings.

## API boundary

The control facade exposes domain records and operations. Other areas do not
receive SQLite handles or issue SQL. Read-only metrics use a WAL snapshot and
must remain available while the worker owns the exclusive mutation lock.
