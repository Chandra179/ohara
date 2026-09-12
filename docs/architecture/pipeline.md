# Pipeline and stages

The pipeline coordinates work without owning vendor implementations. It claims
jobs from the control plane, receives behavioral ports, runs stages, and records
outcomes.

## Stage flow

1. **Scrape** fetches labeled content through the engine and stores the raw
   payload.
2. **Clean** extracts primary text, normalizes it, and returns accepted,
   duplicate, or quality-rejected outcomes.
3. **Chunk and vectorize** creates bounded, breadcrumb-aware embedding input,
   records chunk rows, and writes model-scoped vectors.
4. **Extract graph** validates structured evidence, resolves typed entities,
   stages triplets, and writes mentions and aggregated facts.
5. **Retrieve** is described separately because it serves both the operator CLI
   and the local UI query surface.

The graph stage stops the chain when graph extraction is disabled; vectorized
documents remain searchable through lexical and vector retrieval.

## Boundaries

Stages receive only the ports and validated configuration they need. Provider
assembly, boot compatibility checks, default fallbacks, and recovery ordering
are runtime concerns. Prompt/schema parsing is separate from graph side effects.
Synthesis is separate from retrieval so ranked evidence remains useful when an
LLM is unavailable.

## Consistency protocol

SQLite and LadybugDB do not share a transaction. The worker records intent and
execution state before knowledge writes, advances durable milestones only after
knowledge success, and replays deletion intents knowledge-first. Re-chunking is
delete-first. Replay-safe identities and upserts make interrupted work safe to
retry. The worker's runtime lock prevents operator mutations from racing with
cross-store changes.
