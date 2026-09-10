import { describe, expect, it } from "vitest";
import { createMockApi, type DashboardSnapshot } from "./client";

describe("mock API boundary", () => {
  it("returns a copy of the typed dashboard snapshot", async () => {
    const snapshot: DashboardSnapshot = {
      serviceStatus: "healthy",
      indexedCount: 12,
      processingCount: 1,
      failedCount: 0,
      queue: [
        {
          id: "doc-1",
          name: "Notes.md",
          status: "indexed",
          updatedAt: "just now",
        },
      ],
    };

    const result = await createMockApi(snapshot).getDashboard();

    expect(result).toEqual(snapshot);
    expect(result.queue).not.toBe(snapshot.queue);
  });
});
