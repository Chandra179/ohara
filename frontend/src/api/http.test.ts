import { describe, expect, it, vi } from "vitest";
import { ApiRequestError, createHttpApi } from "./http";

describe("HTTP API boundary", () => {
  it("queues a topic and preserves result status", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(
        JSON.stringify({
          topic: "september 2026 news",
          requested: 2,
          discovered: 2,
          enqueued: 1,
          duplicates: 1,
          documents: [
            {
              title: "Fresh story",
              sourceUrl: "https://example.test/fresh",
              documentId: "doc-1",
              jobId: "job-1",
              status: "enqueued",
            },
            {
              title: "Existing story",
              sourceUrl: "https://example.test/existing",
              documentId: "doc-2",
              jobId: null,
              status: "duplicate",
            },
          ],
        }),
        { status: 200 },
      ),
    );
    const api = createHttpApi({ fetcher });

    await expect(api.scrapeTopic("september 2026 news", 2)).resolves.toMatchObject({
      discovered: 2,
      documents: [{ status: "enqueued" }, { status: "duplicate" }],
      enqueued: 1,
    });
    expect(fetcher).toHaveBeenCalledWith(
      "/api/topics/scrape",
      expect.objectContaining({
        body: JSON.stringify({ limit: 2, topic: "september 2026 news" }),
        method: "POST",
      }),
    );
  });

  it("maps the overview read model and service status from health", async () => {
    const fetcher = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            documentsByStatus: { INDEXED: 4, NEW: 1 },
            queue: [
              {
                documentId: "doc-1",
                documentStatus: "NEW",
                error: null,
                jobId: "job-1",
                jobStatus: "PENDING",
                sourceUrl: "https://example.test",
                stage: "SCRAPE",
                title: "Example",
                updatedAt: "now",
              },
            ],
          }),
          { status: 200 },
        ),
      )
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            controlStore: "available",
            diagnostics: [],
            embedder: "available",
            knowledgeStore: "available",
            llm: "unavailable",
            reranker: "identity",
            status: "degraded",
            worker: {
              currentJobId: null,
              currentStage: null,
              lastError: null,
              lastHeartbeatAt: "2026-09-12 12:00:00",
              processId: 42,
              stale: false,
              startedAt: "2026-09-12 11:55:00",
              state: "ready",
              status: "available",
              workerId: "worker-test",
            },
          }),
          { status: 200 },
        ),
      );
    const api = createHttpApi({ fetcher });

    await expect(api.getDashboard()).resolves.toMatchObject({
      documentsByStatus: { INDEXED: 4, NEW: 1 },
      queue: [{ id: "job-1", jobStatus: "PENDING" }],
      serviceStatus: "degraded",
    });
  });

  it("maps cursor-paginated documents without inventing a document type", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(
        JSON.stringify({
          items: [
            {
              chunkCount: 12,
              createdAt: "2026-09-11 10:00:00",
              error: null,
              id: "doc-1",
              lastProcessedAt: "2026-09-11 10:30:00",
              sourceUrl: "https://example.test/notes",
              status: "INDEXED",
              title: "Notes",
            },
          ],
          nextCursor: "doc-1",
        }),
        { status: 200 },
      ),
    );
    const api = createHttpApi({ fetcher });

    await expect(
      api.listDocuments({ limit: 2, search: "notes", status: "INDEXED" }),
    ).resolves.toMatchObject({
      items: [{ id: "doc-1", status: "INDEXED", title: "Notes" }],
      nextCursor: "doc-1",
    });
    expect(fetcher).toHaveBeenCalledWith(
      "/api/documents?limit=2&status=INDEXED&search=notes",
      expect.objectContaining({ headers: { accept: "application/json" } }),
    );
  });

  it("loads entity review candidates and their preview", async () => {
    const response = {
      candidateA: { aliases: 2, id: "a", name: "Ohara", type: "PRODUCT" },
      candidateB: { aliases: 3, id: "b", name: "O'Hara", type: "PRODUCT" },
      id: "1",
      score: 0.91,
    };
    const fetcher = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(new Response(JSON.stringify([response]), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify(response), { status: 200 }));
    const api = createHttpApi({ fetcher });

    await expect(api.getEntityReviews()).resolves.toEqual([response]);
    await expect(api.previewEntityMerge("1")).resolves.toEqual({
      candidateA: response.candidateA,
      candidateB: response.candidateB,
      reviewId: "1",
      score: response.score,
    });
  });

  it("requests metrics from the local Rust API", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(
        JSON.stringify({
          capturedAt: "now",
          documentsByStatus: {},
          dueForRecrawl: 0,
          eventsByOutcome: {},
          eventsByStage: {},
          jobsByStageStatus: {},
          llmUsage: {
            calls: 0,
            completionTokens: 0,
            estimatedCostMicros: 0,
            failedCalls: 0,
            promptTokens: 0,
            successfulCalls: 0,
          },
          pendingErReviews: 0,
          rawBytes: 0,
          rawFiles: 0,
          rawMaxAgeDays: null,
          rawMaxBytes: null,
        }),
        { status: 200 },
      ),
    );
    const api = createHttpApi({ fetcher });

    await expect(api.getMetrics()).resolves.toMatchObject({ capturedAt: "now" });
    expect(fetcher).toHaveBeenCalledWith(
      "/api/metrics",
      expect.objectContaining({
        headers: { accept: "application/json" },
      }),
    );
  });

  it("maps Rust query responses into the UI query states", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(
        JSON.stringify({
          answer: "A grounded answer.",
          availability: "available",
          citations: ["chunk-1"],
          chunks: [{ chunkId: "chunk-1", score: 0.9, text: "Evidence" }],
          grounding: "grounded",
          reranker: "identity",
        }),
        { status: 200 },
      ),
    );
    const api = createHttpApi({ fetcher });

    await expect(api.queryKnowledgeBase("question")).resolves.toMatchObject({
      answer: "A grounded answer.",
      availability: "available",
      citations: [{ id: "chunk-1", title: "chunk-1" }],
      grounding: "grounded",
      reranker: "identity",
    });
    expect(fetcher).toHaveBeenCalledWith(
      "/api/query",
      expect.objectContaining({
        body: JSON.stringify({ query: "question" }),
        method: "POST",
      }),
    );
  });

  it("preserves unavailable and ungrounded query states from the API", async () => {
    const unavailableFetcher = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(
        JSON.stringify({
          answer: null,
          availability: "unavailable",
          citations: [],
          chunks: [],
          grounding: "ungrounded",
          reranker: "identity",
        }),
        { status: 200 },
      ),
    );
    const unavailableApi = createHttpApi({ fetcher: unavailableFetcher });

    await expect(unavailableApi.queryKnowledgeBase("question")).resolves.toMatchObject({
      availability: "unavailable",
      grounding: "ungrounded",
    });

    const ungroundedFetcher = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(
        JSON.stringify({
          answer: null,
          availability: "available",
          citations: [],
          chunks: [{ chunkId: "chunk-1", score: 0.9, text: "Evidence" }],
          grounding: "ungrounded",
          reranker: "identity",
        }),
        { status: 200 },
      ),
    );
    const ungroundedApi = createHttpApi({ fetcher: ungroundedFetcher });

    await expect(ungroundedApi.queryKnowledgeBase("question")).resolves.toMatchObject({
      availability: "available",
      grounding: "ungrounded",
    });
  });

  it("rejects malformed metrics before the Operations view can crash", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(JSON.stringify({ capturedAt: "now", llmUsage: { successful_calls: 1 } }), {
        status: 200,
      }),
    );
    const api = createHttpApi({ fetcher });

    await expect(api.getMetrics()).rejects.toThrow("invalid metrics response");
  });

  it("surfaces structured API errors", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(JSON.stringify({ error: "query unavailable" }), { status: 503 }),
    );
    const api = createHttpApi({ fetcher });

    await expect(api.queryKnowledgeBase("question")).rejects.toEqual(
      new ApiRequestError("query unavailable", 503),
    );
  });
});
