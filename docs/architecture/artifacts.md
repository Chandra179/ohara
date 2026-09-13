# Artifact contracts

The processes communicate through JSON files in a shared data directory. Each
producer writes to a temporary filename and renames it into place. Consumers
read only completed `.json` files, remove an inbox file after successful
processing, and move failed inputs into the stage-specific dead-letter
directory.

Operators can replay failed inputs with a bounded command such as
`make replay STAGE=indexer LIMIT=10`. Replay moves at most the requested number
of JSON files back into the owning inbox; it does not bypass validation or
provider error handling.

Operators can rebuild derived stores with `make rebuild` after stopping the
five Rust processes. The command recreates Qdrant and FalkorDB, then requeues
durable clean and indexed artifacts through their normal inboxes. It preserves
the source artifacts so the rebuild remains repeatable.

The handoff sequence is:

```text
scraper:  RawArtifact → inbox/cleaning
cleaning: CleanArtifact → inbox/indexer
indexer:  IndexedArtifact → inbox/graph
retrieval: catalog + Qdrant results → browser response
```

The shared layout includes `inbox/cleaning`, `inbox/indexer`, and `inbox/graph`
for pending work, plus matching `dead-letter/cleaning`, `dead-letter/indexer`,
and `dead-letter/graph` directories for failed inputs.

Every artifact carries `document_id`. Indexed chunks carry `chunk_id`,
`document_id`, `source_url`, `title`, `text`, and `sequence`. A chunk id is the
SHA-256 hash of `document_id:sequence`.

Each handoff also carries `schema_version`. The current value is `1`; consumers
reject another version and move the incompatible input to dead letter instead
of guessing how to decode it. Contract changes require a new version and
coordinated producer/consumer updates.

Raw artifact paths are relative to the shared data directory. This keeps the
handoff portable when producers and consumers run in different containers.

The JSON shape is intentionally duplicated as a small process-local type in
each package. There is no shared `contracts` crate. The persisted fields and
their meaning are kept here as the cross-team Interface; changing one requires
updating the producer, consumer, and this document together.

Version-one fixtures for the three handoffs live beside this contract. Each
consumer parses its fixture in its package tests, so a contract edit fails a
focused test before it reaches another process.

`make pipeline-fixture` additionally runs the real scraper, cleaning, indexer,
and retrieval processes against local deterministic test doubles and verifies
the complete scrape-to-query handoff.
