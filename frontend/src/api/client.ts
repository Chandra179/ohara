export type ServiceStatus = "degraded" | "healthy" | "offline";

export type DocumentStatus =
  | "ARCHIVED"
  | "CLEANED"
  | "FAILED"
  | "FAILED_QUALITY"
  | "INDEXED"
  | "NEW"
  | "SCRAPED"
  | "VECTORIZED";

export type JobStatus = "DEAD" | "DONE" | "PENDING" | "RUNNING";

export type EntityType =
  | "CONCEPT"
  | "EVENT"
  | "LOCATION"
  | "ORGANIZATION"
  | "PERSON"
  | "PRODUCT";

export interface QueueItem {
  documentId: string;
  documentStatus: DocumentStatus;
  error: string | null;
  id: string;
  jobStatus: Exclude<JobStatus, "DONE">;
  sourceUrl: string;
  stage: string;
  title: string | null;
  updatedAt: string;
}

export interface DashboardSnapshot {
  documentsByStatus: Partial<Record<DocumentStatus, number>>;
  queue: QueueItem[];
  serviceStatus: ServiceStatus;
}

export type TopicQueueStatus = "duplicate" | "enqueued";

export interface TopicQueueDocument {
  documentId: string;
  jobId: string | null;
  sourceUrl: string;
  status: TopicQueueStatus;
  title: string;
}

export interface TopicScrapeResult {
  discovered: number;
  documents: TopicQueueDocument[];
  duplicates: number;
  enqueued: number;
  requested: number;
  topic: string;
}

export type ComponentStatus = "available" | "unavailable";

export type HealthComponent = "controlStore" | "embedder" | "knowledgeStore" | "llm" | "worker";

export type WorkerState = "failed" | "ready" | "running" | "starting" | "stopped" | "stopping";

export interface ReadinessDiagnostic {
  action: string;
  component: HealthComponent;
  message: string;
}

export interface WorkerSnapshot {
  currentJobId: string | null;
  currentStage: string | null;
  lastError: string | null;
  lastHeartbeatAt: string | null;
  processId: number | null;
  stale: boolean;
  startedAt: string | null;
  state: WorkerState | null;
  status: ComponentStatus;
  workerId: string | null;
}

export interface HealthSnapshot {
  controlStore: ComponentStatus;
  diagnostics: ReadinessDiagnostic[];
  embedder: ComponentStatus;
  knowledgeStore: ComponentStatus;
  llm: ComponentStatus;
  reranker: "identity";
  status: ServiceStatus;
  worker: WorkerSnapshot;
}

export interface DocumentRecord {
  chunkCount: number;
  createdAt: string;
  error: string | null;
  id: string;
  lastProcessedAt: string | null;
  sourceUrl: string;
  status: DocumentStatus;
  title: string | null;
}

export interface DocumentPage {
  items: DocumentRecord[];
  nextCursor: string | null;
}

export interface DocumentListOptions {
  cursor?: string;
  limit?: number;
  search?: string;
  status?: DocumentStatus;
}

export interface Citation {
  id: string;
  location: string;
  source: string;
  title: string;
}

export type QueryAvailability = "available" | "unavailable";

export type QueryGrounding = "grounded" | "ungrounded";

export interface QueryResult {
  answer: string;
  availability: QueryAvailability;
  citations: Citation[];
  grounding: QueryGrounding;
  reranker: "identity";
}

export interface EntityRecord {
  aliases: number;
  id: string;
  name: string;
  type: EntityType;
}

export interface EntityReview {
  candidateA: EntityRecord;
  candidateB: EntityRecord;
  id: string;
  score: number | null;
}

export interface MergePreview {
  candidateA: EntityRecord;
  candidateB: EntityRecord;
  reviewId: string;
  score: number | null;
}

/** Counts for one category in the operator metrics snapshot. */
export type MetricCounts = Record<string, number>;

/** Durable LLM usage totals exposed by the operator metrics contract. */
export interface LlmUsageSnapshot {
  calls: number;
  completionTokens: number;
  estimatedCostMicros: number;
  failedCalls: number;
  promptTokens: number;
  successfulCalls: number;
}

/** Read-only operator metrics returned by the metrics API. */
export interface MetricsSnapshot {
  capturedAt: string;
  documentsByStatus: MetricCounts;
  dueForRecrawl: number;
  eventsByOutcome: MetricCounts;
  eventsByStage: MetricCounts;
  jobsByStageStatus: Record<string, MetricCounts>;
  llmUsage: LlmUsageSnapshot;
  pendingErReviews: number;
  rawBytes: number;
  rawFiles: number;
  rawMaxAgeDays: number | null;
  rawMaxBytes: number | null;
}

export interface OharaApi {
  getDashboard(): Promise<DashboardSnapshot>;
  getEntityReviews(): Promise<EntityReview[]>;
  getHealth(): Promise<HealthSnapshot>;
  getMetrics(): Promise<MetricsSnapshot>;
  listDocuments(options?: DocumentListOptions): Promise<DocumentPage>;
  previewEntityMerge(reviewId: string): Promise<MergePreview>;
  queryKnowledgeBase(question: string): Promise<QueryResult>;
  scrapeTopic(topic: string, limit?: number): Promise<TopicScrapeResult>;
}

const DEFAULT_DASHBOARD: DashboardSnapshot = {
  documentsByStatus: {
    FAILED: 3,
    INDEXED: 2847,
    NEW: 2,
  },
  serviceStatus: "healthy",
  queue: [
    {
      documentId: "doc-1",
      documentStatus: "NEW",
      error: null,
      id: "job-1",
      jobStatus: "RUNNING",
      sourceUrl: "https://example.com/product-notes",
      stage: "SCRAPE",
      title: "Product Notes Q1",
      updatedAt: "2 min ago",
    },
    {
      documentId: "doc-2",
      documentStatus: "SCRAPED",
      error: null,
      id: "job-2",
      jobStatus: "PENDING",
      sourceUrl: "https://example.com/research-draft",
      stage: "CLEAN",
      title: "Research Draft",
      updatedAt: "4 min ago",
    },
    {
      documentId: "doc-3",
      documentStatus: "FAILED",
      error: "The extraction provider failed",
      id: "job-3",
      jobStatus: "DEAD",
      sourceUrl: "https://example.com/old-notes",
      stage: "EXTRACT",
      title: "Old Notes",
      updatedAt: "25 min ago",
    },
  ],
};

const DEFAULT_HEALTH: HealthSnapshot = {
  controlStore: "available",
  diagnostics: [],
  embedder: "available",
  knowledgeStore: "available",
  llm: "available",
  reranker: "identity",
  status: "healthy",
  worker: {
    currentJobId: null,
    currentStage: null,
    lastError: null,
    lastHeartbeatAt: "2026-09-12 12:00:00",
    processId: null,
    stale: false,
    startedAt: "2026-09-12 11:55:00",
    state: "ready",
    status: "available",
    workerId: "worker-demo",
  },
};

const DEFAULT_DOCUMENTS: DocumentRecord[] = [
  {
    chunkCount: 0,
    createdAt: "2026-09-11 10:00:00",
    error: null,
    id: "doc-1",
    lastProcessedAt: null,
    sourceUrl: "https://example.com/product-notes",
    status: "NEW",
    title: "Product Notes Q1",
  },
  {
    chunkCount: 0,
    createdAt: "2026-09-11 09:00:00",
    error: null,
    id: "doc-2",
    lastProcessedAt: "2026-09-11 09:30:00",
    sourceUrl: "https://example.com/research-draft",
    status: "SCRAPED",
    title: "Research Draft",
  },
  {
    chunkCount: 24,
    createdAt: "2026-09-10 12:00:00",
    error: null,
    id: "doc-3",
    lastProcessedAt: "2026-09-10 12:30:00",
    sourceUrl: "https://example.com/design-system",
    status: "INDEXED",
    title: "Design System v2",
  },
  {
    chunkCount: 16,
    createdAt: "2026-09-10 11:00:00",
    error: null,
    id: "doc-4",
    lastProcessedAt: "2026-09-10 11:30:00",
    sourceUrl: "https://example.com/meeting-transcript",
    status: "INDEXED",
    title: "Meeting Transcript",
  },
  {
    chunkCount: 12,
    createdAt: "2026-09-09 14:00:00",
    error: null,
    id: "doc-5",
    lastProcessedAt: "2026-09-09 14:30:00",
    sourceUrl: "https://example.com/ohara-overview",
    status: "INDEXED",
    title: "Ohara Overview",
  },
  {
    chunkCount: 0,
    createdAt: "2026-09-09 12:00:00",
    error: "The document did not meet the quality threshold",
    id: "doc-6",
    lastProcessedAt: "2026-09-09 12:30:00",
    sourceUrl: "https://example.com/old-notes",
    status: "FAILED_QUALITY",
    title: "Old Notes",
  },
  {
    chunkCount: 0,
    createdAt: "2026-09-08 12:00:00",
    error: "The fetch job exhausted its retry budget",
    id: "doc-7",
    lastProcessedAt: "2026-09-08 12:30:00",
    sourceUrl: "https://example.com/failed-import",
    status: "FAILED",
    title: "Failed Import",
  },
  {
    chunkCount: 8,
    createdAt: "2026-09-07 12:00:00",
    error: null,
    id: "doc-8",
    lastProcessedAt: "2026-09-07 12:30:00",
    sourceUrl: "https://example.com/archived-notes",
    status: "ARCHIVED",
    title: "Archived Notes",
  },
];

const DEFAULT_ENTITY_REVIEWS: EntityReview[] = [
  {
    candidateA: { aliases: 2, id: "entity-1", name: "Ohara", type: "PRODUCT" },
    candidateB: { aliases: 3, id: "entity-2", name: "Ohara", type: "PRODUCT" },
    id: "review-1",
    score: 0.94,
  },
  {
    candidateA: { aliases: 2, id: "entity-3", name: "Local-first", type: "CONCEPT" },
    candidateB: { aliases: 4, id: "entity-4", name: "Local first", type: "CONCEPT" },
    id: "review-2",
    score: 0.88,
  },
  {
    candidateA: { aliases: 1, id: "entity-5", name: "Semantic search", type: "CONCEPT" },
    candidateB: { aliases: 3, id: "entity-6", name: "Semantic retrieval", type: "CONCEPT" },
    id: "review-3",
    score: 0.76,
  },
  {
    candidateA: { aliases: 1, id: "entity-7", name: "Product", type: "CONCEPT" },
    candidateB: { aliases: 5, id: "entity-8", name: "Product", type: "PRODUCT" },
    id: "review-4",
    score: 0.61,
  },
  {
    candidateA: { aliases: 2, id: "entity-9", name: "Knowledge graph", type: "CONCEPT" },
    candidateB: { aliases: 5, id: "entity-10", name: "Knowledge graph", type: "CONCEPT" },
    id: "review-5",
    score: 0.9,
  },
];

const DEFAULT_METRICS: MetricsSnapshot = {
  capturedAt: "2026-09-11T00:00:00.000Z",
  documentsByStatus: {
    INDEXED: 2847,
    SCRAPED: 12,
    FAILED: 3,
  },
  dueForRecrawl: 8,
  eventsByOutcome: {
    DONE: 112,
    RETRY: 4,
    SKIP: 9,
  },
  eventsByStage: {
    CLEAN: 31,
    EXTRACT: 28,
    SCRAPE: 37,
    VECTORIZE: 29,
  },
  jobsByStageStatus: {
    CLEAN: { DONE: 2842, PENDING: 3 },
    EXTRACT: { DONE: 2819, PENDING: 9, RUNNING: 2 },
    SCRAPE: { DONE: 2860, PENDING: 8, RUNNING: 4 },
    VECTORIZE: { DONE: 2834, PENDING: 12, DEAD: 3 },
  },
  llmUsage: {
    calls: 84,
    completionTokens: 12_480,
    estimatedCostMicros: 0,
    failedCalls: 3,
    promptTokens: 46_200,
    successfulCalls: 81,
  },
  pendingErReviews: 5,
  rawBytes: 18_874_368,
  rawFiles: 42,
  rawMaxAgeDays: 30,
  rawMaxBytes: 536_870_912,
};

/** Creates deterministic offline data for local UI development and tests. */
export function createMockApi(
  snapshot: DashboardSnapshot = DEFAULT_DASHBOARD,
  documents: DocumentRecord[] = DEFAULT_DOCUMENTS,
  entityReviews: EntityReview[] = DEFAULT_ENTITY_REVIEWS,
  metricsSnapshot: MetricsSnapshot = DEFAULT_METRICS,
): OharaApi {
  const pendingReviews = entityReviews.map((review) => cloneEntityReview(review));
  const previewEntityMerge = async (reviewId: string): Promise<MergePreview> => {
    const review = pendingReviews.find((candidate) => candidate.id === reviewId);
    if (!review) {
      throw new Error("Entity review was not found");
    }

    return {
      candidateA: { ...review.candidateA },
      candidateB: { ...review.candidateB },
      reviewId,
      score: review.score,
    };
  };

  return {
    async getDashboard() {
      return {
        ...snapshot,
        queue: snapshot.queue.map((item) => ({ ...item })),
      };
    },
    async getEntityReviews() {
      return pendingReviews.map((review) => cloneEntityReview(review));
    },
    async getHealth() {
      return { ...DEFAULT_HEALTH };
    },
    async getMetrics() {
      return cloneMetrics(metricsSnapshot);
    },
    async listDocuments(options = {}) {
      const normalizedSearch = options.search?.trim().toLowerCase() ?? "";
      const filtered = documents
        .filter((document) => options.status === undefined || document.status === options.status)
        .filter(
          (document) =>
            normalizedSearch.length === 0 ||
            document.title?.toLowerCase().includes(normalizedSearch) === true ||
            document.sourceUrl.toLowerCase().includes(normalizedSearch),
        )
        .sort((left, right) => right.id.localeCompare(left.id));
      const start = options.cursor === undefined
        ? 0
        : Math.max(0, filtered.findIndex((document) => document.id === options.cursor) + 1);
      const limit = options.limit ?? 25;
      const pageItems = filtered.slice(start, start + limit + 1);
      const hasNext = pageItems.length > limit;
      if (hasNext) {
        pageItems.pop();
      }
      return {
        items: pageItems.map((document) => ({ ...document })),
        nextCursor: hasNext ? pageItems.at(-1)?.id ?? null : null,
      };
    },
    previewEntityMerge,
    async scrapeTopic(topic, limit) {
      const normalizedTopic = topic.trim();
      const requested = limit ?? 5;
      const documents = [
        {
          documentId: "topic-doc-1",
          jobId: "topic-job-1",
          sourceUrl: "https://example.com/news/one",
          status: "enqueued" as const,
          title: `${normalizedTopic} · lead story`,
        },
        {
          documentId: "topic-doc-2",
          jobId: "topic-job-2",
          sourceUrl: "https://example.com/news/two",
          status: "enqueued" as const,
          title: `${normalizedTopic} · second story`,
        },
        {
          documentId: "topic-doc-3",
          jobId: null,
          sourceUrl: "https://example.com/news/three",
          status: "duplicate" as const,
          title: `${normalizedTopic} · already saved`,
        },
      ].slice(0, requested);
      const duplicates = documents.filter((document) => document.status === "duplicate").length;
      return {
        discovered: documents.length,
        documents,
        duplicates,
        enqueued: documents.length - duplicates,
        requested,
        topic: normalizedTopic,
      };
    },
    async queryKnowledgeBase(question) {
      const normalizedQuestion = question.toLowerCase();
      if (normalizedQuestion.includes("error")) {
        throw new Error("The query service is unavailable");
      }

      if (normalizedQuestion.includes("offline")) {
        return {
          answer: "",
          availability: "unavailable",
          citations: [],
          grounding: "ungrounded",
          reranker: "identity",
        };
      }

      if (normalizedQuestion.includes("ungrounded")) {
        return {
          answer: "The local index did not return enough evidence for this question.",
          availability: "available",
          citations: [],
          grounding: "ungrounded",
          reranker: "identity",
        };
      }

      return {
        answer:
          "Ohara is a local-first personal knowledge workbench that helps you collect, understand, and connect your information. It keeps your data on your machine and combines semantic search with structured entities.",
        availability: "available",
        citations: [
          {
            id: "citation-1",
            location: "p. 1–3",
            source: "Product documentation",
            title: "ohara-product-guide.pdf",
          },
          {
            id: "citation-2",
            location: "p. 4–6",
            source: "Notes",
            title: "local-first-principles.md",
          },
          {
            id: "citation-3",
            location: "p. 2",
            source: "Personal notes",
            title: "knowledge-workflow.txt",
          },
        ],
        grounding: "grounded",
        reranker: "identity",
      };
    },
  };
}

function cloneEntityReview(review: EntityReview): EntityReview {
  return {
    ...review,
    candidateA: { ...review.candidateA },
    candidateB: { ...review.candidateB },
  };
}

function cloneMetrics(metrics: MetricsSnapshot): MetricsSnapshot {
  return {
    ...metrics,
    documentsByStatus: { ...metrics.documentsByStatus },
    eventsByOutcome: { ...metrics.eventsByOutcome },
    eventsByStage: { ...metrics.eventsByStage },
    jobsByStageStatus: Object.fromEntries(
      Object.entries(metrics.jobsByStageStatus).map(([stage, statuses]) => [stage, { ...statuses }]),
    ),
    llmUsage: { ...metrics.llmUsage },
  };
}
