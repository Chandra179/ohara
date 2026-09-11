export type ServiceStatus = "degraded" | "healthy" | "offline";

export type QueueStatus = "failed" | "indexed" | "processing";

export type DocumentType = "DOCX" | "MD" | "PDF" | "TXT";

export type EntityType = "CONCEPT" | "ORGANIZATION" | "PERSON" | "PRODUCT";

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

export type ComponentStatus = "available" | "unavailable";

export interface HealthSnapshot {
  controlStore: ComponentStatus;
  knowledgeStore: ComponentStatus;
  llm: ComponentStatus;
  status: ServiceStatus;
}

export interface DocumentRecord {
  id: string;
  name: string;
  source: string;
  status: QueueStatus;
  type: DocumentType;
  updatedAt: string;
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
}

export interface EntityRecord {
  name: string;
  references: number;
  type: EntityType;
}

export interface EntityReview {
  duplicate: EntityRecord;
  id: string;
  matchCount: number;
  reason: string;
  winner: EntityRecord;
}

export interface MergePreview {
  merged: EntityRecord;
  reviewId: string;
  loser: EntityRecord;
  winner: EntityRecord;
}

export interface MergeResult {
  mergedAt: string;
  preview: MergePreview;
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
  listDocuments(): Promise<DocumentRecord[]>;
  mergeEntities(reviewId: string): Promise<MergeResult>;
  previewEntityMerge(reviewId: string): Promise<MergePreview>;
  queryKnowledgeBase(question: string): Promise<QueryResult>;
}

const DEFAULT_DASHBOARD: DashboardSnapshot = {
  serviceStatus: "healthy",
  indexedCount: 2847,
  processingCount: 12,
  failedCount: 3,
  queue: [
    { id: "queue-1", name: "Product Notes Q1.pdf", status: "processing", updatedAt: "2 min ago" },
    { id: "queue-2", name: "Research Draft.docx", status: "processing", updatedAt: "4 min ago" },
    { id: "queue-3", name: "Design System v2.pdf", status: "indexed", updatedAt: "12 min ago" },
    { id: "queue-4", name: "Meeting Transcript.txt", status: "indexed", updatedAt: "18 min ago" },
    { id: "queue-5", name: "Old Notes.pdf", status: "failed", updatedAt: "25 min ago" },
  ],
};

const DEFAULT_HEALTH: HealthSnapshot = {
  controlStore: "available",
  knowledgeStore: "available",
  llm: "available",
  status: "healthy",
};

const DEFAULT_DOCUMENTS: DocumentRecord[] = [
  { id: "doc-1", name: "Product Notes Q1.pdf", type: "PDF", source: "Local", status: "processing", updatedAt: "2 min ago" },
  { id: "doc-2", name: "Research Draft.docx", type: "DOCX", source: "Local", status: "processing", updatedAt: "4 min ago" },
  { id: "doc-3", name: "Design System v2.pdf", type: "PDF", source: "Local", status: "indexed", updatedAt: "12 min ago" },
  { id: "doc-4", name: "Meeting Transcript.txt", type: "TXT", source: "Local", status: "indexed", updatedAt: "18 min ago" },
  { id: "doc-5", name: "Ohara Overview.md", type: "MD", source: "Local", status: "indexed", updatedAt: "1 hour ago" },
  { id: "doc-6", name: "Knowledge Workflow.pdf", type: "PDF", source: "Local", status: "indexed", updatedAt: "3 hours ago" },
  { id: "doc-7", name: "Ideas & Notes.md", type: "MD", source: "Local", status: "indexed", updatedAt: "5 hours ago" },
  { id: "doc-8", name: "Readme.txt", type: "TXT", source: "Local", status: "indexed", updatedAt: "1 day ago" },
];

const DEFAULT_ENTITY_REVIEWS: EntityReview[] = [
  {
    duplicate: { name: "Ohara", references: 2, type: "PRODUCT" },
    id: "review-1",
    matchCount: 2,
    reason: "Potential duplicate",
    winner: { name: "Ohara", references: 3, type: "PRODUCT" },
  },
  {
    duplicate: { name: "Local-first", references: 2, type: "CONCEPT" },
    id: "review-2",
    matchCount: 2,
    reason: "Potential duplicate",
    winner: { name: "Local-first", references: 4, type: "CONCEPT" },
  },
  {
    duplicate: { name: "Semantic search", references: 1, type: "CONCEPT" },
    id: "review-3",
    matchCount: 2,
    reason: "Needs confirmation",
    winner: { name: "Semantic retrieval", references: 3, type: "CONCEPT" },
  },
  {
    duplicate: { name: "Product", references: 1, type: "CONCEPT" },
    id: "review-4",
    matchCount: 3,
    reason: "Ambiguous entity",
    winner: { name: "Product", references: 5, type: "PRODUCT" },
  },
  {
    duplicate: { name: "Knowledge graph", references: 2, type: "CONCEPT" },
    id: "review-5",
    matchCount: 2,
    reason: "Potential duplicate",
    winner: { name: "Knowledge graph", references: 5, type: "CONCEPT" },
  },
];

const DEFAULT_METRICS: MetricsSnapshot = {
  capturedAt: "2026-09-11T00:00:00.000Z",
  documentsByStatus: {
    INDEXED: 2847,
    PROCESSING: 12,
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

/**
 * Creates the temporary adapter used until a live HTTP adapter is connected.
 * Production data should be provided by an implementation of [`OharaApi`].
 */
export function createMockApi(
  snapshot: DashboardSnapshot = DEFAULT_DASHBOARD,
  documents: DocumentRecord[] = DEFAULT_DOCUMENTS,
  entityReviews: EntityReview[] = DEFAULT_ENTITY_REVIEWS,
  metricsSnapshot: MetricsSnapshot = DEFAULT_METRICS,
): OharaApi {
  let pendingReviews = entityReviews.map((review) => cloneEntityReview(review));
  const previewEntityMerge = async (reviewId: string): Promise<MergePreview> => {
    const review = pendingReviews.find((candidate) => candidate.id === reviewId);
    if (!review) {
      throw new Error("Entity review was not found");
    }

    return {
      merged: {
        ...review.winner,
        references: review.winner.references + review.duplicate.references,
      },
      reviewId,
      loser: { ...review.duplicate },
      winner: { ...review.winner },
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
    async listDocuments() {
      return documents.map((document) => ({ ...document }));
    },
    async mergeEntities(reviewId) {
      const preview = await previewEntityMerge(reviewId);
      pendingReviews = pendingReviews.filter((review) => review.id !== reviewId);
      return { mergedAt: new Date().toISOString(), preview };
    },
    previewEntityMerge,
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
        };
      }

      if (normalizedQuestion.includes("ungrounded")) {
        return {
          answer: "The local index did not return enough evidence for this question.",
          availability: "available",
          citations: [],
          grounding: "ungrounded",
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
      };
    },
  };
}

function cloneEntityReview(review: EntityReview): EntityReview {
  return {
    ...review,
    duplicate: { ...review.duplicate },
    winner: { ...review.winner },
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
