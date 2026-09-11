import { describe, expect, it, vi } from "vitest";
import { ApiRequestError, createHttpApi } from "./http";

describe("HTTP API boundary", () => {
  it("requests metrics from the local Rust API", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(JSON.stringify({ capturedAt: "now" }), { status: 200 }),
    );
    const api = createHttpApi({ fetcher });

    await expect(api.getMetrics()).resolves.toEqual({ capturedAt: "now" });
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
          citations: ["chunk-1"],
          chunks: [{ chunkId: "chunk-1", score: 0.9, text: "Evidence" }],
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
    });
    expect(fetcher).toHaveBeenCalledWith(
      "/api/query",
      expect.objectContaining({
        body: JSON.stringify({ query: "question" }),
        method: "POST",
      }),
    );
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
