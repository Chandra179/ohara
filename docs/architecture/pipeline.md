# Pipeline and stages

The pipeline coordinates work through behavioral ports. It does not create
database clients, network clients, or model providers.

## Stage flow

1. Scrape fetches labeled content and stores the raw payload.
2. Clean extracts primary text and applies language, quality, duplicate, and
   paywall outcomes.
3. Chunk/vectorize creates bounded embedding input, records chunk rows, and
   writes Qdrant vectors.
4. Extract graph validates evidence, resolves typed entities, and writes
   FalkorDB mentions and facts.
5. Retrieve combines full-text, vector, and graph results before reranking and
   optional cited synthesis.

Graph extraction is optional. Vectorized documents remain searchable through
full-text and vector retrieval when it is disabled.

## Consistency

SQLite and the knowledge services do not share a transaction. Durable intent,
deterministic identities, idempotent upserts, and knowledge-first deletion make
retries and process interruption safe. The worker lock prevents operator
mutations from racing with worker writes.
