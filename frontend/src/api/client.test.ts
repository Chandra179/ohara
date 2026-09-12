import { describe, expect, it } from "vitest";
import { createMockApi, type DashboardSnapshot } from "./client";

describe("mock API boundary", () => {
  it("returns a copy of the typed dashboard snapshot", async () => {
    const snapshot: DashboardSnapshot = {
      documentsByStatus: { INDEXED: 12 },
      serviceStatus: "healthy",
      queue: [
        {
          documentId: "doc-1",
          documentStatus: "INDEXED",
          error: null,
          id: "job-1",
          jobStatus: "RUNNING",
          sourceUrl: "https://example.test/notes",
          stage: "SCRAPE",
          title: "Notes",
          updatedAt: "just now",
        },
      ],
    };

    const result = await createMockApi(snapshot).getDashboard();

    expect(result).toEqual(snapshot);
    expect(result.queue).not.toBe(snapshot.queue);
  });

  it("returns an isolated operator metrics snapshot", async () => {
    const api = createMockApi();

    const result = await api.getMetrics();
    result.jobsByStageStatus.SCRAPE.PENDING = 0;

    const nextResult = await api.getMetrics();

    expect(nextResult.jobsByStageStatus.SCRAPE.PENDING).toBe(8);
    expect(nextResult.llmUsage).toMatchObject({ calls: 84, successfulCalls: 81 });
    expect(nextResult.rawMaxBytes).toBe(536_870_912);
  });

  it("paginates document summaries and preserves backend statuses", async () => {
    const api = createMockApi();

    const page = await api.listDocuments({ limit: 2 });
    expect(page.items).toHaveLength(2);
    expect(page.items[0].status).toBe("ARCHIVED");
    expect(page.nextCursor).toBe("doc-7");

    const nextPage = await api.listDocuments({ cursor: page.nextCursor ?? undefined, limit: 2 });
    expect(nextPage.items).toHaveLength(2);
    expect(nextPage.items[0].id).toBe("doc-6");
  });

  it("supports the query result states used by the UI", async () => {
    const api = createMockApi();

    await expect(api.queryKnowledgeBase("offline provider")).resolves.toMatchObject({
      availability: "unavailable",
      grounding: "ungrounded",
    });
    await expect(api.queryKnowledgeBase("ungrounded question")).resolves.toMatchObject({
      availability: "available",
      grounding: "ungrounded",
    });
    await expect(api.queryKnowledgeBase("error response")).rejects.toThrow(
      "query service is unavailable",
    );
  });

});
