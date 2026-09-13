# Ohara frontend

The frontend is a React + TypeScript + Vite application. It runs locally with
npm and communicates only with the `retrieval` Rust process.

## Development

```sh
npm install
npm run dev
```

For the live local stack, run from the repository root:

```sh
make providers
make scraper
make cleaning
make indexer
make graph
make retrieval
make frontend
```

`make dev` starts all five Rust processes and Vite together. `make docker-up`
builds and runs the Rust processes and local databases in Compose; frontend is
still started with npm.

For browser checks against a running local stack, keep `make dev` or Compose
running in another terminal and run `npm run e2e:live`. The default `npm run
e2e` suite uses the frontend's deterministic mock adapter.

## HTTP interface

The Vite development proxy forwards `/api` to retrieval at
`http://127.0.0.1:3000`. The frontend uses these retrieval routes:

- `GET /api/health`
- `GET /api/overview`
- `GET /api/documents`
- `GET /api/metrics`
- `POST /api/topics/scrape`
- `POST /api/query`
- `GET /api/entities/reviews`
- `GET /api/entities/reviews/{id}/preview`

Topic requests include the selected maximum article count:

```json
{
  "topic": "september 2026 news",
  "limit": 5
}
```

Retrieval forwards the request to scraper. The frontend does not know about
the shared artifact directory, Qdrant, FalkorDB, Ollama, or Docker.

## Checks

```sh
npm run lint
npm test -- --run
npm run build
PLAYWRIGHT_EXECUTABLE_PATH=/usr/bin/google-chrome npm run e2e:live
```
