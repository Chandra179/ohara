# Operator services

Operator services are process-local workflows over the public plane facades.
They cover query, backup, document lifecycle, offline entity merge, entity GC,
raw retention, and read-only metrics.

Mutating workflows share the runtime lock with the worker. Backups checkpoint
and snapshot SQLite, copy the closed knowledge/runtime artifacts into staging,
write a manifest, and publish only after the complete snapshot is ready.

Deletion is an intent-first workflow. Requeue, archive, delete, merge, GC, and
prune are idempotent and preserve the control/knowledge consistency protocol.
Read-only metrics use a control-plane snapshot and do not require the exclusive
mutation lock.

The local HTTP API exposes health, metrics, overview, documents, query, and
read-only entity-review previews. It is an API-only process boundary and does
not supervise the ingestion worker. Lifecycle mutations remain a planned
contract so confirmation, authorization, idempotency, and failure behavior can
be specified before browser-triggered writes are enabled.
