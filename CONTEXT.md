# Ohara domain context

Ohara turns a topic into a local knowledge base through five independently
runnable process modules. The processes exchange explicit JSON artifacts and
use Qdrant, FalkorDB, and Ollama as local providers.

## Domain language

**Document** — a normalized source URL and its raw, clean, and indexed forms.

**Raw artifact** — fetched HTML plus source metadata produced by scraper.

**Clean artifact** — extracted, sanitized Markdown produced by cleaning.

**Chunk** — one canonical bounded piece of clean Markdown produced by indexer.

**Indexed artifact** — a document's chunks and evidence metadata produced by
indexer and consumed by graph and retrieval.

**Entity** — a named person, organization, place, event, concept, or product
recognized in indexed evidence.

**Mention** — a graph relationship connecting a chunk to an entity.

**Grounded answer** — non-empty Ollama output returned with one or more exact
chunk ids from the retrieved evidence.

**Artifact contract** — the persisted JSON fields, invariants, and ordering
rules at a process seam.

## Relationships

- A topic produces zero or more Documents.
- A Document produces one Raw artifact, one Clean artifact, and one Indexed
  artifact.
- An Indexed artifact contains ordered Chunks.
- A Chunk can produce Entity Mentions in FalkorDB.
- Retrieval uses Chunks as evidence for a grounded answer.

## Architectural decisions

- Each Rust package has one process responsibility and no local package
  dependency on another process.
- JSON artifacts are the local handoff medium; there is no shared `contracts`
  or `control-client` crate.
- Producers own their artifact shape and consumers validate required fields.
- Deterministic hashes make document and chunk replay idempotent.
- Qdrant and FalkorDB are derived stores and can be rebuilt from artifacts.
- The frontend talks only to retrieval.
- Scraper discovery and fetch settings are loaded from `scraper/config.yaml`;
  environment overrides are limited to deployment values and secrets.
- Loopback binding is the default until authentication exists.

## Intentional limitations

The current graph extractor is a lightweight capitalized-phrase baseline.
Retrieval combines Qdrant, local full-text overlap, and bounded graph-path
signals; richer structured extraction, entity-resolution threshold
measurement, HNSW evaluation, richer failure queues, and multi-host durable
messaging are future work.
