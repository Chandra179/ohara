# Ohara

Ohara is a private knowledge-building application. It takes documents, learns
their important text and relationships, and lets you ask questions over the
resulting knowledge base.

It is designed for personal-scale collections on one computer. Local files and
local models are the default. The built-in language-model path is local Ollama;
cloud providers are planned and cloud egress is disabled today.

## What can it do?

- Fetch web pages while respecting robots rules and protecting the local network.
- Remove boilerplate and keep useful headings, lists, tables, and code.
- Reject content that is too short, paywalled, low quality, duplicated, or in an
  unsupported language.
- Split long documents into meaningful, token-sized pieces.
- Search exact words, semantic meaning, and entity relationships together.
- Identify people, organizations, places, events, concepts, and products.
- Preserve evidence for every extracted fact and answer citation.
- Retry temporary failures and recover safely after a process restart.
- Inspect progress, usage, failed work, and maintenance state locally.

The product interface includes views for overview, documents, queries, entity
review, and operations. The live read-only connection covers health, metrics,
overview, documents, query, and entity-review previews. Lifecycle actions remain
deliberately separate until their confirmation and idempotency contract is
ready.

## The architecture in one picture

```text
                         user
                           │
                     local interface
                           │
                    runtime and operators
                           │
       ┌───────────────────┼───────────────────┐
       │                   │                   │
   control             pipeline            knowledge
 durable state        coordinates work    vectors + graph
       ▲                   │                   ▲
       │                   │                   │
       └─────────────── fetch engine ──────────┘
                         network input
```

The control area remembers what must happen and what already happened. The
pipeline coordinates work. The fetch engine handles external network input.
The knowledge area holds a rebuildable search and relationship index. The local
interface presents results and operations without accessing storage directly.

Each area has one responsibility and communicates through small behavioral
interfaces. This keeps provider changes, storage changes, and user-interface
changes from spreading through the whole application.

## How a document becomes knowledge

1. **Fetch** — retrieve a page and record its final location, status, and
   validators for future recrawls.
2. **Clean** — extract the primary content, normalize it, and apply quality
   checks.
3. **Chunk** — split the content at headings and natural text boundaries. Each
   piece carries a short heading breadcrumb so its context is not lost.
4. **Embed** — convert each piece into a numeric representation of its meaning.
5. **Extract** — find typed entities and relationships, validate them, and keep
   the original piece as evidence.
6. **Index** — store word-search data, vectors, entity links, and fact edges.
7. **Answer** — retrieve the strongest evidence and optionally write a bounded,
   citation-preserving answer.

## Algorithms

### URL normalization and duplicate detection

URLs are normalized before registration: irrelevant fragments and common tracking
parameters are removed, host and scheme casing is standardized, and query
parameters are made deterministic. This prevents the same page from being
registered under several superficial URL forms.

Clean content also receives a hash. If two documents have the same clean hash,
the system can avoid doing the expensive downstream work twice.

### Content cleaning and quality checks

The cleaner prefers the main article content over navigation, advertisements,
and boilerplate. It preserves useful structure, removes unsafe embedded data,
and normalizes Unicode and whitespace.

Quality checks are normal decisions, not system failures. A document can be
rejected for insufficient content, paywall markers, boilerplate-only content,
or unsupported language.

### Token-aware chunking

Long content is first divided by headings. Oversized sections are split at
paragraphs, lines, and sentence boundaries, in that order. Tables and fenced
code remain whole. Small overlap between neighboring pieces helps preserve
meaning across a split.

The size limit is measured using the embedding model's tokenizer, including the
heading breadcrumb. This is more reliable than counting characters or spaces.

### Embeddings and vector search

An embedding is a list of numbers where nearby lists represent similar meaning.
Ohara compares the query embedding with document embeddings using cosine
similarity. The current implementation scans the exact vector index, which is
simple and accurate for the intended personal-scale collection.

HNSW is a faster approximate index that may be added later. It must first prove
that its recall is close enough to exact search on the evaluation set.

### Entity resolution

Entity matching is type-aware. An exact typed alias is preferred. If no alias
matches, the system compares names within the same type and can compare their
embeddings. Ambiguous matches are sent to review instead of being merged
silently. Entity merges happen as explicit, auditable operations.

### GraphRAG retrieval

Ohara combines three kinds of evidence:

1. **Full-text search** finds exact words, names, identifiers, and phrases.
2. **Vector search** finds semantically similar passages.
3. **Graph search** finds passages that mention query entities and facts related
   to them.

The lists are combined with reciprocal-rank fusion, which rewards results that
appear near the top of several lists. A reranker may reorder the candidates. If
the reranker fails, the fused order remains available.

The answer writer receives only a bounded set of passages and labeled facts. An
answer is accepted only when it is non-empty and cites exact evidence ids.
Otherwise the application returns the ranked passages instead of inventing an
uncited answer.

## Reliability and privacy

Every processing stage has a durable job state. Temporary failures use bounded
retries and backoff. Leases allow interrupted work to be recovered. Writes are
idempotent, so replaying work does not duplicate knowledge.

The durable control data is authoritative. Search vectors and graph data are
derived and can be rebuilt. This is important because the two storage systems
cannot commit one shared transaction; recovery uses an explicit safe ordering.

Fetched pages are treated as untrusted data. They are never executed as code.
Network access is restricted, and cloud language-model use is disabled by
default.

## Current limitations

- The default vector search is exact rather than HNSW-accelerated.
- The production query path currently uses the deterministic identity reranker
  baseline.
- The live interface connection covers health, metrics, overview, documents,
  query, and entity-review previews; lifecycle mutations are next.
- Entity-resolution thresholds are conservative configuration defaults and still
  need measurement on larger, ambiguous collections.
- Symspell correction, HyDE query expansion, stage throughput dashboards,
  embedding migration, and cloud language-model providers are future work.
