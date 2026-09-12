# Ohara system architecture

Ohara turns web content into a local, searchable knowledge base. A worker
fetches and processes articles; the API and frontend expose status, search,
retrieval, and citations.

## Planes

| Plane | Owns | Does not own |
| :--- | :--- | :--- |
| Control | SQLite documents, jobs, audit, identities, and usage | HTTP, vectors, or graph queries |
| Engine | outbound HTTP, fetch policy, and topic discovery | durable state or knowledge writes |
| Knowledge | Qdrant vectors and FalkorDB graph data | queue scheduling or SQLite schema |
| Pipeline | stage order, orchestration, and recovery sequencing | vendor clients and transport |

Runtime composition connects the planes. The frontend and local API are
transport surfaces and never access a datastore directly.

The worker is the only application writer for knowledge data. The API uses
read-only knowledge adapters for readiness and query traffic. Qdrant and
FalkorDB are local services managed by Docker Compose; SQLite remains the
durable control system and the knowledge services remain rebuildable indexes.

## Data flow

```text
topic → fetch → clean → chunk → embed → extract entities/facts
                                      ↓
                           lexical + vector + graph retrieval
                                      ↓
                           ranked evidence and cited answer
```

The control plane records each stage and its outcome. Chunk and triplet
identities are deterministic, so retries are safe. Cross-store writes do not
share a transaction: durable intent and idempotent writes make interruption
recoverable, and deletion removes knowledge data before the SQLite cascade.

## Main invariants

1. Each datastore has one owning plane.
2. Pipeline code depends on behavioral ports, never vendor types.
3. Knowledge data is derived and can be rebuilt from durable control data.
4. Content-hash identities and upserts make replay safe.
5. Domain outcomes such as duplicate, paywalled, and low-quality content are
   values; infrastructure failures are typed retryable errors.
6. The local API binds to loopback until an authenticated deployment boundary
   exists.

## Retrieval and graph

Qdrant provides model-scoped cosine vector search. FalkorDB stores typed
entities, chunk mentions, and aggregated fact edges. Retrieval combines SQLite
full-text search, Qdrant nearest-neighbor results, and FalkorDB graph context,
then applies the configured reranking and citation-preserving synthesis.

The current embedding model is English-first and 384-dimensional. HNSW is
Qdrant's index mechanism; its recall and latency still need measurement before
changing retrieval defaults. Entity-resolution thresholds also remain a
measured-quality task, not a storage migration concern.

## Components

Focused contracts and operational details live in:

- [control plane](architecture/control-plane.md)
- [fetch engine](architecture/engine.md)
- [knowledge plane](architecture/knowledge-plane.md)
- [pipeline](architecture/pipeline.md)
- [retrieval](architecture/retrieval.md)
- [language-model services](architecture/llm.md)
- [runtime composition](architecture/runtime.md)
- [operator services](architecture/operations.md)
- [frontend and API](architecture/ui-api.md)
- [testing and build](architecture/testing-and-build.md)
