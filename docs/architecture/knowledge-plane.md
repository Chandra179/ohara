# Knowledge plane

The knowledge plane owns the embedded LadybugDB index: model-scoped vectors,
entity-name vectors, graph nodes, graph relationships, and graph traversals.
It is a rebuildable projection of durable control data, not a second source of
truth.

## Stored shape

- Chunk vectors are isolated by embedding model identity.
- Entity-name vectors support typed entity resolution and query-entity lookup.
- Chunk-to-entity mention relationships support graph retrieval.
- Entity-to-entity fact edges aggregate support counts and bounded evidence.

All writes are idempotent. Deleting a document removes its vectors and graph
links. Folding an entity rewires relationships and removes the loser in one
knowledge-store transaction. Entity deletion is refused while live graph
relationships remain.

## Vector posture

The current implementation uses exact in-engine cosine KNN. The bundled engine
does not provide HNSW, so HNSW is a future implementation behind the same port,
not a current configuration switch. Any replacement must preserve collection
isolation, deterministic upserts, filtering, and the delete-then-KNN
postcondition.

## Recovery

The knowledge index can be rebuilt from stored chunk embedding text, triplet
evidence, and control-plane identity mappings. Cross-store ordering is owned by
the pipeline recovery flow; the knowledge plane exposes operations but does not
decide when a control mutation is safe. When Ladybug reports a truncated frozen
WAL checkpoint tail, opening the store retries with the incomplete tail
discarded and removes that stale checkpoint. Other invalid artifacts remain
unavailable and require an explicit rebuild or repair action.
