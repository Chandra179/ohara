# Indexer module

The indexer owns the canonical representation of a clean document. It splits
Markdown into bounded overlapping chunks, derives deterministic chunk ids, and
embeds each chunk with `bge-small-en-v1.5` through `fastembed`.

The same chunk record is written to the indexed artifact and to Qdrant with the
text, title, URL, document id, and chunk id in the payload. The indexed artifact
is the graph input, which prevents graph extraction from inventing a second
chunking policy.

The indexer accepts clean artifact schema version `1` and publishes indexed
artifact schema version `1`. An incompatible input is rejected before model or
Qdrant work begins.

Qdrant is a derived store. If its volume is removed, the indexer can replay the
clean artifacts. The current collection uses 384-dimensional cosine vectors;
HNSW remains a measured optimization rather than an implicit default.

The process-boundary harness may set `OHARA_EMBEDDING_MODE=deterministic` to
replace model inference with a stable local test vector. The default mode
remains `fastembed` and uses the configured local embedding model.
