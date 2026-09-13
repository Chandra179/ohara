# Completed root TODO items

Archived on 2026-09-13 after the five-process workspace migration.

## P0 — architecture and operability

- [x] Split the monolithic Rust crate into `scraper`, `cleaning`, `indexer`,
  `graph`, and `retrieval` packages.
- [x] Give every Rust package its own source tree and Dockerfile.
- [x] Define atomic JSON artifact handoffs and deterministic document/chunk ids.
- [x] Replace the old SQLite/Ladybug runtime with Qdrant, FalkorDB, and the
  shared artifact directory.
- [x] Keep the frontend-facing HTTP interface in retrieval and keep frontend
  execution local with npm.
- [x] Add Compose builds, local commands, and small per-process CPU/RAM limits.
- [x] Keep the Makefile aligned with the five-process workspace and remove
  monolith-only run, worker, and port-management targets.
- [x] Remove the legacy root implementation.
- [x] Remove legacy root migrations and tests.
- [x] Add durable per-stage dead-letter directories for failed artifacts.
- [x] Add an explicit, stage-scoped replay command with a per-run item limit.
- [x] Add a rebuild command that recreates derived Qdrant/FalkorDB stores and
  requeues durable clean and indexed artifacts safely.

## P1 — stage contracts

- [x] Add contract fixtures for every artifact version and reject incompatible
  versions before processing.
- [x] Add per-document processing state and failure reason to catalog updates.
- [x] Add input/output counters and latency metrics for each process.
- [x] Add a deterministic process-boundary fixture harness for the
  scrape-to-query path.
