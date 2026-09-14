# What Ohara does

Ohara is a private, local GraphRAG application. Give it a web topic, let it
collect the resulting articles, and ask questions over the cleaned knowledge
base. Answers include the chunk ids used as evidence.

## Features

- Bounded topic discovery through configurable Bing News, Google News, Brave,
  DuckDuckGo, or custom RSS providers.
- Raw HTML fetching with URL normalization and response limits.
- Main-article extraction and Markdown cleaning.
- Canonical overlapping chunks and local `bge-small-en-v1.5` embeddings.
- Qdrant semantic search.
- FalkorDB entity and mention graph.
- Ollama answer synthesis using `phi4-mini:latest` by default.
- Health, document, queue, metrics, and query views in the local frontend.
- Rebuildable derived data: the artifact directory is the durable handoff.

## How the stages cooperate

```text
topic
  ↓
scraper        raw HTML
  ↓
cleaning       clean Markdown
  ↓
indexer        chunks + vectors
  ├──────────→  Qdrant
  ↓
graph          entities + mentions → FalkorDB
  ↓
retrieval      ranked evidence + local answer
  ↓
frontend       user interface
```

The stages are separate Rust processes. Each has one responsibility, can be
restarted independently, and communicates through explicit JSON artifact
handoffs. The frontend is a local npm process and only talks to retrieval.

## Algorithms in plain language

The scraper removes URL fragments and common tracking parameters before hashing
the URL into a stable document id. The cleaner keeps the primary article body
and rejects very short results. The indexer splits long text into overlapping
pieces so a passage keeps enough neighboring context, then converts each piece
into a 384-number embedding.

Retrieval converts the question with the same embedding model and combines
Qdrant similarity, local full-text overlap, and matching chunk-to-entity graph
paths. It sends the bounded fused evidence to Ollama and accepts the answer
only when the model returns non-empty text. When synthesis is unavailable or
produces no answer, the ranked evidence remains visible.

The graph extractor recognizes typed fields and typed lexical cues for people,
organizations, locations, events, concepts, and products. It normalizes names,
resolves explicit aliases, and stores idempotent typed mentions. Untyped
capitalized phrases are not treated as entities.

## Privacy and limitations

Fetched content and models remain on the local machine by default. Ollama is
local; no cloud model is configured. Search providers and destination pages are
external network inputs and are treated as untrusted data. Brave requires an
API key, and DuckDuckGo discovery requires the optional Obscura browser binary.

The current vector search defaults to Qdrant exact search. An explicit HNSW
mode is available after benchmarking; the benchmark compares both modes on
the same workload and keeps exact search unless HNSW retains at least 98% of
exact recall@10 and improves p95 latency. The repository includes deterministic
golden retrieval and entity-resolution evaluations; the selected entity
candidate threshold is `0.98`, while approximate graph merges remain disabled
until deliberately adopted. Production-scale relevance labeling, richer
failure queues, and lifecycle operations remain future work.
