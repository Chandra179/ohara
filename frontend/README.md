# Ohara frontend

The frontend is a React + TypeScript + Vite application. It uses a typed mock
adapter by default and has an opt-in HTTP adapter for the Rust local operator
and bounded topic-queue surface.

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
PLAYWRIGHT_EXECUTABLE_PATH=/usr/bin/google-chrome npm run e2e:live
```

The HTTP adapter keeps API base URL configuration at the frontend boundary and
does not expose SQLite, LadybugDB, or runtime filesystem paths to components.

## Read and topic-queue contracts

`GET /api/overview` returns `documentsByStatus` using the durable control-plane
statuses and a bounded `queue` projection. The HTTP adapter combines that
projection with `/api/health` for the Overview service badge.

`POST /api/topics/scrape` accepts a bounded topic request:

```json
{
  "topic": "september 2026 news",
  "limit": 5
}
```

The topic is trimmed and limited to 200 characters. `limit` defaults to 5 and
must be between 1 and 10. The response reports discovered, newly enqueued, and
duplicate results, including each document and job identity. The endpoint is
loopback-only through the local API and queues work; the separate worker must
run before pages are fetched and indexed.

`GET /api/documents` returns cursor-paginated summaries:

```json
{
  "items": [
    {
      "id": "document-id",
      "sourceUrl": "https://example.com",
      "title": "Example",
      "status": "INDEXED",
      "chunkCount": 12,
      "createdAt": "2026-09-11 10:00:00",
      "lastProcessedAt": "2026-09-11 10:30:00",
      "error": null
    }
  ],
  "nextCursor": null
}
```

The optional query parameters are `limit`, `cursor`, `status`, and `search`.
Status values are the control-plane values: `NEW`, `SCRAPED`, `CLEANED`,
`VECTORIZED`, `INDEXED`, `FAILED_QUALITY`, `FAILED`, and `ARCHIVED`.

`GET /api/entities/reviews` lists pending candidates. Selecting one loads
`GET /api/entities/reviews/{id}/preview`, which returns both canonical entity
candidates and the detector score. Entity merge mutations remain unavailable
until their confirmation and idempotency contract is implemented.

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
  "availability": "available",
  "citations": ["chunk-id"],
  "chunks": [{ "chunkId": "...", "score": 0.91, "text": "..." }],
  "grounding": "grounded",
  "reranker": "identity"
}
```

`availability` distinguishes an unreachable language model from an available
model that could not produce a grounded answer. `grounding` is authoritative;
the frontend does not infer it from whether `answer` or `citations` happen to
be present. `reranker` exposes the current deterministic identity baseline.

`GET /api/health` returns component statuses, worker lifecycle data, and a
`diagnostics` array. A truncated Ladybug WAL checkpoint tail is recovered during
store open when it is safe to discard the incomplete tail; other invalid
artifacts remain unavailable with a rebuild action. Worker data reports the latest boot state and heartbeat;
the `stale` flag identifies a worker that stopped reporting within the lease
horizon. Each diagnostic identifies the unavailable component, explains the
problem, and provides the next action. Missing local embedding files, invalid
knowledge artifacts, and absent workers are reported this way instead of
leaving a page blank.
