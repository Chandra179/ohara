# Ohara frontend

The frontend is a React + TypeScript + Vite application. It uses a typed mock
adapter by default and has an opt-in HTTP adapter for the initial Rust API
slice.

## Local development

```sh
npm install
npm run dev
```

The default development mode uses the mock adapter. From the repository root,
start the Rust API and Vite in separate terminals:

```sh
make backend
make frontend
```

For frontend-only work, `npm run dev` keeps using the mock adapter. `make frontend`
keeps the requested port strict, waits for the API, and proxies `/api` to the
configured `API_BIND`. The combined `make dev` launcher remains available. Set
`VITE_OHARA_API_BASE_URL` only when the API is hosted at another origin; it is
not a secret.

Run the frontend checks with:

```sh
npm test
npm run lint
npm run build
PLAYWRIGHT_EXECUTABLE_PATH=/usr/bin/google-chrome npm run e2e
```

The HTTP adapter keeps API base URL configuration at the frontend boundary and
does not expose SQLite, LadybugDB, or runtime filesystem paths to components.

## Metrics contract

The Operations screen consumes the read-only `GET /api/metrics` contract below.
The Rust server exposes this endpoint with the frontend's `camelCase` transport
names; `createMockApi()` provides a fixture with the same shape. The existing
Rust `ohara metrics --json` command remains a separate CLI contract and emits
the same fields in `snake_case`.

```json
{
  "capturedAt": "2026-09-11T00:00:00.000Z",
  "documentsByStatus": { "INDEXED": 2847 },
  "jobsByStageStatus": { "SCRAPE": { "PENDING": 8, "DONE": 2860 } },
  "eventsByOutcome": { "DONE": 112 },
  "eventsByStage": { "SCRAPE": 37 },
  "pendingErReviews": 5,
  "dueForRecrawl": 8,
  "rawFiles": 42,
  "rawBytes": 18874368,
  "rawMaxBytes": 536870912,
  "rawMaxAgeDays": 30,
  "llmUsage": {
    "calls": 84,
    "successfulCalls": 81,
    "failedCalls": 3,
    "promptTokens": 46200,
    "completionTokens": 12480,
    "estimatedCostMicros": 0
  }
}
```

Counts are non-negative integers. `rawMaxBytes` and `rawMaxAgeDays` are
nullable when retention limits are not configured. `estimatedCostMicros` is an
estimated USD amount in millionths of a dollar, not a billing guarantee.

The contract intentionally has no throughput or latency fields. The UI must
not draw operational charts until the backend records and exposes those
counters.

## Query contract

`POST /api/query` accepts a JSON body with a required `query` and optional
`topK`. It returns immutable chunk citations and ranked evidence:

```json
{
  "answer": "...",
  "citations": ["chunk-id"],
  "chunks": [{ "chunkId": "...", "score": 0.91, "text": "..." }]
}
```

An absent `answer` is a valid ungrounded response. The frontend maps it to the
existing fallback state without fabricating document metadata.
