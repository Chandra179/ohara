# Graph module

The graph process consumes indexed chunks and writes idempotent `Chunk`,
`Entity`, and `MENTIONS` records to FalkorDB. Its current bounded extractor
recognizes capitalized noun phrases as a lightweight baseline. The graph write
uses deterministic entity ids and `MERGE`, so replay does not duplicate nodes
or mentions.

It accepts indexed artifact schema version `1` and rejects incompatible
versions before writing the graph.

The process owns graph persistence and does not alter chunks or vector payloads.
The extractor can later be replaced by a structured LLM Adapter behind this
process without changing the indexer or retrieval artifact seams.
