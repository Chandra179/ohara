# Frontend TODO

The frontend is a local React + TypeScript + Vite application. It talks only to
the retrieval process and remains outside Docker for local development.

## P0 — current workflow

- [x] Keep loading, empty, unavailable, and error states for every retrieval
  route.
- [x] Wire overview, documents, query, entities, operations, and topic queueing
  to the retrieval HTTP interface.
- [x] Send the selected article limit with topic requests.
- [x] Automatically expire success and info notifications after ten seconds.
- [x] Avoid a request for every character in document search; debounce input.

## P1 — new process architecture

- [ ] Show scraper, cleaning, indexer, graph, and retrieval readiness as
  separate process indicators.
- [ ] Display the artifact stage and latest failure reason for each document.
- [ ] Add a retry action after the backend exposes durable retry contracts.
- [ ] Show indexing progress from stage metrics when those metrics exist.

## P2 — quality and safety

- [ ] Add query citation links when retrieval exposes source metadata.
- [ ] Add a clear stale-data indicator when the shared artifact directory is
  unavailable.
- [ ] Add local authentication/CSRF protection before binding retrieval beyond
  loopback.
- [ ] Add accessibility and live-stack checks to CI.

## P3 — quality lab

- [ ] Add retrieval and entity-resolution evaluation views after their backend
  measurement contracts are stable.
- [ ] Add HNSW and threshold comparison views only from measured backend data.
