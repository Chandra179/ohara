# Process metrics

Each process writes one durable snapshot to
`data/state/<process>.metrics.json`. The file is replaced atomically after a
completed measurement, so retrieval can read it while a process is running.
Counters continue across process restarts when the existing snapshot is valid.

The snapshot shape is:

```json
{
  "process": "indexer",
  "inputCount": 12,
  "outputCount": 11,
  "failureCount": 1,
  "totalLatencyMs": 840,
  "lastLatencyMs": 72,
  "maxLatencyMs": 190,
  "updatedAt": "1789290000"
}
```

`inputCount` is the number of work items measured. `outputCount` is the number
that completed and published their result. `failureCount` is the number that
failed or were skipped. `totalLatencyMs`, `lastLatencyMs`, and `maxLatencyMs`
measure the same work items; an average can be calculated as total latency
divided by input count.

The process meanings are intentionally local:

- scraper: discovered provider results, newly published raw artifacts, and
  skipped results;
- cleaning: raw artifacts, clean artifacts, and failed artifacts;
- indexer: clean artifacts, indexed artifacts, and failed artifacts;
- graph: indexed artifacts, graph writes, and failed artifacts;
- retrieval: HTTP requests, successful responses, and non-success responses.

Retrieval exposes all five snapshots under the `stages` field of
`GET /api/metrics`. A missing process file is reported as a zero snapshot so
the operations page remains useful while a process is stopped.
