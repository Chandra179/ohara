# Architecture modules

Read [system architecture](../ARCHITECTURE.md) first. These pages describe the
five process modules and the seams between them:

- [Scraper](scraper.md) — discovery, URL policy, and raw HTML.
- [Cleaning](cleaning.md) — article extraction and quality normalization.
- [Indexer](indexer.md) — canonical chunks, embeddings, and Qdrant.
- [Graph](graph.md) — entity/mention extraction and FalkorDB.
- [Retrieval](retrieval.md) — frontend HTTP, search, and cited synthesis.
- [Artifact contracts](artifacts.md) — the JSON handoff shapes and retry rules.
- [Build and operations](operations.md) — Compose, resource budgets, and checks.
