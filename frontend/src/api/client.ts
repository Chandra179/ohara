export type ServiceStatus = "degraded" | "healthy" | "offline";

export type QueueStatus = "failed" | "indexed" | "processing";

export interface QueueItem {
  id: string;
  name: string;
  status: QueueStatus;
  updatedAt: string;
}

export interface DashboardSnapshot {
  serviceStatus: ServiceStatus;
  indexedCount: number;
  processingCount: number;
  failedCount: number;
  queue: QueueItem[];
}

export interface OharaApi {
  getDashboard(): Promise<DashboardSnapshot>;
}

const DEFAULT_DASHBOARD: DashboardSnapshot = {
  serviceStatus: "healthy",
  indexedCount: 0,
  processingCount: 0,
  failedCount: 0,
  queue: [],
};

/**
 * Creates the temporary adapter used until the frontend API contract exists.
 * Production data should be provided by an implementation of [`OharaApi`].
 */
export function createMockApi(
  snapshot: DashboardSnapshot = DEFAULT_DASHBOARD,
): OharaApi {
  return {
    async getDashboard() {
      return {
        ...snapshot,
        queue: snapshot.queue.map((item) => ({ ...item })),
      };
    },
  };
}
