# What Ohara does

Ohara is a private, local GraphRAG application. Give it a web topic, let it
collect the resulting articles, and ask questions over the cleaned knowledge
base. Answers include the chunk ids used as evidence.

## Features

- Bounded topic discovery through Bing News RSS.
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

Retrieval converts the question with the same embedding model and asks Qdrant
for the nearest chunks. It sends the bounded evidence to Ollama and accepts the
answer only when the model returns non-empty text. When synthesis is unavailable
or produces no answer, the ranked evidence remains visible.

The graph baseline identifies bounded capitalized noun phrases and stores them
as idempotent entity mentions. A structured entity extractor can replace this
implementation later without changing the artifact seam.

## Privacy and limitations

Fetched content and models remain on the local machine by default. Ollama is
local; no cloud model is configured. Bing News RSS and destination pages are
external network inputs and are treated as untrusted data.

The current vector search uses Qdrant's exact endpoint, the graph extractor is a
lightweight baseline, and entity-resolution threshold measurement, HNSW
benchmarking, richer evaluation, and lifecycle operations remain future work.
