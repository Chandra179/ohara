import type {
  ComponentStatus,
  DashboardSnapshot,
  DocumentListOptions,
  DocumentPage,
  DocumentRecord,
  DocumentStatus,
  EntityRecord,
  EntityReview,
  EntityType,
  HealthSnapshot,
  HealthComponent,
  HealthProcess,
  HealthProvider,
  JobStatus,
  MetricCounts,
  MergePreview,
  MetricsSnapshot,
  OharaApi,
  QueryAvailability,
  QueryGrounding,
  QueryResult,
  ReadinessDiagnostic,
  TopicQueueDocument,
  TopicQueueStatus,
  TopicScrapeResult,
} from "./client";

interface HttpApiOptions {
  baseUrl?: string;
  fetcher?: typeof fetch;
}

interface QueryResponsePayload {
  answer: string | null;
  availability: QueryAvailability;
  citations: string[];
  chunks: Array<{ chunkId: string; score: number; text: string }>;
  grounding: QueryGrounding;
}

interface QueuePayload {
  documentId: string;
  documentStatus: DocumentStatus;
  error: string | null;
  jobId: string;
  jobStatus: Exclude<JobStatus, "DONE">;
  sourceUrl: string;
  stage: string;
  title: string | null;
  updatedAt: string;
}

/** Error returned when the local Rust API rejects or cannot answer a request. */
export class ApiRequestError extends Error {
  readonly status: number;

  constructor(message: string, status: number) {
    super(message);
    this.name = "ApiRequestError";
    this.status = status;
  }
}

/** Creates the browser adapter for the optional local Rust HTTP server. */
export function createHttpApi({ baseUrl = "", fetcher = fetch }: HttpApiOptions = {}): OharaApi {
  const request = createRequest(baseUrl, fetcher);

  return {
    getDashboard: async () => {
      const [overview, health] = await Promise.all([
        request<unknown>("/api/overview").then(parseOverviewPayload),
        request<unknown>("/api/health").then(parseHealthSnapshot),
      ]);
      return { ...overview, serviceStatus: health.status };
    },
    getEntityReviews: () => request<unknown>("/api/entities/reviews").then(parseEntityReviewList),
    getHealth: () => request<unknown>("/api/health").then(parseHealthSnapshot),
    getMetrics: () => request<unknown>("/api/metrics").then(parseMetricsSnapshot),
    listDocuments: (options) =>
      request<unknown>(documentsPath(options)).then(parseDocumentPage),
    previewEntityMerge: (reviewId) =>
      request<unknown>(
        `/api/entities/reviews/${encodeURIComponent(reviewId)}/preview`,
      ).then(parseMergePreview),
    queryKnowledgeBase: async (question) => {
      const response = await request<unknown>("/api/query", {
        body: JSON.stringify({ query: question }),
        headers: { "content-type": "application/json" },
        method: "POST",
      });
      return mapQueryResponse(parseQueryResponse(response));
    },
    scrapeTopic: async (topic, limit) => {
      const response = await request<unknown>("/api/topics/scrape", {
        body: JSON.stringify({ limit, topic }),
        headers: { "content-type": "application/json" },
        method: "POST",
      });
      return parseTopicScrapeResponse(response);
    },
  };
}

function createRequest(baseUrl: string, fetcher: typeof fetch) {
  const normalizedBaseUrl = baseUrl.replace(/\/$/, "");

  return async function request<T>(path: string, init?: RequestInit): Promise<T> {
    let response: Response;
    try {
      response = await fetcher(`${normalizedBaseUrl}${path}`, {
        ...init,
        headers: {
          accept: "application/json",
          ...init?.headers,
        },
      });
    } catch (error: unknown) {
      const message = error instanceof Error ? error.message : "The local API is unreachable";
      throw new ApiRequestError(message, 0);
    }

    const payload = await readPayload(response);
    if (!response.ok) {
      throw new ApiRequestError(errorMessage(payload, response.status), response.status);
    }
    return payload as T;
  };
}

async function readPayload(response: Response): Promise<unknown> {
  const text = await response.text();
  if (text.length === 0) {
    return undefined;
  }

  try {
    return JSON.parse(text) as unknown;
  } catch {
    throw new ApiRequestError("The local API returned invalid JSON", response.status);
  }
}

function errorMessage(payload: unknown, status: number): string {
  if (isErrorPayload(payload)) {
    return payload.error;
  }
  return `The local API request failed with HTTP ${status}`;
}

function isErrorPayload(payload: unknown): payload is { error: string } {
  return (
    typeof payload === "object" &&
    payload !== null &&
    "error" in payload &&
    typeof payload.error === "string"
  );
}

function documentsPath(options: DocumentListOptions = {}): string {
  const params = new URLSearchParams();
  if (options.limit !== undefined) {
    params.set("limit", String(options.limit));
  }
  if (options.cursor !== undefined) {
    params.set("cursor", options.cursor);
  }
  if (options.status !== undefined) {
    params.set("status", options.status);
  }
  if (options.search !== undefined) {
    params.set("search", options.search);
  }
  const query = params.toString();
  return query.length > 0 ? `/api/documents?${query}` : "/api/documents";
}

function parseOverviewPayload(payload: unknown): Omit<DashboardSnapshot, "serviceStatus"> {
  if (!isRecord(payload)) {
    throw invalidResponse("overview");
  }
  const documentsByStatus = parseDocumentCounts(payload.documentsByStatus);
  if (documentsByStatus === undefined) {
    throw invalidResponse("overview");
  }
  const queue = payload.queue;
  if (!Array.isArray(queue) || !queue.every(isQueuePayload)) {
    throw invalidResponse("overview");
  }
  return {
    documentsByStatus,
    queue: queue.map((item) => ({ ...item, id: item.jobId })),
  };
}

function parseTopicScrapeResponse(payload: unknown): TopicScrapeResult {
  if (
    !isRecord(payload) ||
    typeof payload.topic !== "string" ||
    !isCount(payload.requested) ||
    !isCount(payload.discovered) ||
    !isCount(payload.enqueued) ||
    !isCount(payload.duplicates) ||
    !Array.isArray(payload.documents)
  ) {
    throw invalidResponse("topic scrape");
  }
  const documents = payload.documents.map(parseTopicQueueDocument);
  if (documents.some((document) => document === undefined)) {
    throw invalidResponse("topic scrape");
  }
  return {
    discovered: payload.discovered,
    documents: documents as TopicQueueDocument[],
    duplicates: payload.duplicates,
    enqueued: payload.enqueued,
    requested: payload.requested,
    topic: payload.topic,
  };
}

function parseTopicQueueDocument(value: unknown): TopicQueueDocument | undefined {
  if (
    !isRecord(value) ||
    typeof value.documentId !== "string" ||
    !isNullableString(value.jobId) ||
    typeof value.sourceUrl !== "string" ||
    !isTopicQueueStatus(value.status) ||
    typeof value.title !== "string"
  ) {
    return undefined;
  }
  return value as unknown as TopicQueueDocument;
}

function parseDocumentPage(payload: unknown): DocumentPage {
  if (!isRecord(payload) || !Array.isArray(payload.items)) {
    throw invalidResponse("documents");
  }
  const items = payload.items.map(parseDocumentRecord);
  if (
    items.some((item) => item === undefined) ||
    !isNullableString(payload.nextCursor)
  ) {
    throw invalidResponse("documents");
  }
  return { items: items as DocumentRecord[], nextCursor: payload.nextCursor };
}

function parseDocumentRecord(value: unknown): DocumentRecord | undefined {
  if (
    !isRecord(value) ||
    typeof value.id !== "string" ||
    typeof value.sourceUrl !== "string" ||
    !isNullableString(value.title) ||
    !isDocumentStatus(value.status) ||
    !isNumber(value.chunkCount) ||
    typeof value.createdAt !== "string" ||
    !isNullableString(value.lastProcessedAt) ||
    !isNullableString(value.error)
  ) {
    return undefined;
  }
  return value as unknown as DocumentRecord;
}

function isQueuePayload(value: unknown): value is QueuePayload {
  return (
    isRecord(value) &&
    typeof value.documentId === "string" &&
    isDocumentStatus(value.documentStatus) &&
    isNullableString(value.error) &&
    typeof value.jobId === "string" &&
    isJobStatus(value.jobStatus) &&
    typeof value.sourceUrl === "string" &&
    typeof value.stage === "string" &&
    value.stage.length > 0 &&
    isNullableString(value.title) &&
    typeof value.updatedAt === "string"
  );
}

function parseHealthSnapshot(payload: unknown): HealthSnapshot {
  if (!isRecord(payload)) {
    throw invalidResponse("health");
  }

  const status = parseServiceStatus(payload.status);
  const processes = parseHealthComponents<HealthProcess>(
    payload.processes,
    isHealthProcess,
    ["cleaning", "graph", "indexer", "retrieval", "scraper"],
  );
  const providers = parseHealthComponents<HealthProvider>(
    payload.providers,
    isHealthProvider,
    ["artifactStore", "embeddingModel", "falkordb", "ollama", "qdrant"],
  );
  if (
    status === undefined ||
    processes === undefined ||
    providers === undefined ||
    !Array.isArray(payload.diagnostics)
  ) {
    throw invalidResponse("health");
  }

  const diagnostics = payload.diagnostics.map(parseDiagnostic);
  if (diagnostics.some((diagnostic) => diagnostic === undefined)) {
    throw invalidResponse("health");
  }

  return {
    diagnostics: diagnostics as ReadinessDiagnostic[],
    processes,
    providers,
    status,
  };
}

function parseHealthComponents<T extends string>(
  value: unknown,
  isComponent: (value: unknown) => value is T,
  expected: readonly T[],
): Record<T, ComponentStatus> | undefined {
  if (!isRecord(value)) {
    return undefined;
  }
  const entries = Object.entries(value);
  if (
    entries.length !== expected.length ||
    expected.some((key) => !(key in value)) ||
    entries.some(
      ([key, status]) => !isComponent(key) || parseComponentStatus(status) === undefined,
    )
  ) {
    return undefined;
  }
  return Object.fromEntries(entries) as Record<T, ComponentStatus>;
}

function parseEntityReviewList(payload: unknown): EntityReview[] {
  if (!Array.isArray(payload)) {
    throw invalidResponse("entity reviews");
  }
  const reviews = payload.map(parseEntityReview);
  if (reviews.some((review) => review === undefined)) {
    throw invalidResponse("entity reviews");
  }
  return reviews as EntityReview[];
}

function parseMergePreview(payload: unknown): MergePreview {
  const review = parseEntityReview(payload);
  if (review === undefined) {
    throw invalidResponse("entity review preview");
  }
  return {
    candidateA: review.candidateA,
    candidateB: review.candidateB,
    reviewId: review.id,
    score: review.score,
  };
}

function parseEntityReview(value: unknown): EntityReview | undefined {
  if (!isRecord(value)) {
    return undefined;
  }
  const candidateA = parseEntityRecord(value.candidateA);
  const candidateB = parseEntityRecord(value.candidateB);
  if (
    typeof value.id !== "string" ||
    candidateA === undefined ||
    candidateB === undefined ||
    !isNullableScore(value.score)
  ) {
    return undefined;
  }
  return { candidateA, candidateB, id: value.id, score: value.score };
}

function parseEntityRecord(value: unknown): EntityRecord | undefined {
  if (
    !isRecord(value) ||
    typeof value.id !== "string" ||
    typeof value.name !== "string" ||
    !isNumber(value.aliases) ||
    !isEntityType(value.type)
  ) {
    return undefined;
  }
  return value as unknown as EntityRecord;
}

function parseMetricsSnapshot(payload: unknown): MetricsSnapshot {
  if (!isRecord(payload)) {
    throw invalidResponse("metrics");
  }

  const llmUsage = payload.llmUsage;
  if (
    typeof payload.capturedAt !== "string" ||
    !isMetricCounts(payload.documentsByStatus) ||
    !isNumber(payload.dueForRecrawl) ||
    !isMetricCounts(payload.eventsByOutcome) ||
    !isMetricCounts(payload.eventsByStage) ||
    !isStageCounts(payload.jobsByStageStatus) ||
    !isRecord(llmUsage) ||
    !isNumber(llmUsage.calls) ||
    !isNumber(llmUsage.successfulCalls) ||
    !isNumber(llmUsage.failedCalls) ||
    !isNumber(llmUsage.promptTokens) ||
    !isNumber(llmUsage.completionTokens) ||
    !isNumber(llmUsage.estimatedCostMicros) ||
    !isNumber(payload.pendingErReviews) ||
    !isNumber(payload.rawBytes) ||
    !isNumber(payload.rawFiles) ||
    !isNullableNumber(payload.rawMaxAgeDays) ||
    !isNullableNumber(payload.rawMaxBytes)
  ) {
    throw invalidResponse("metrics");
  }

  return payload as unknown as MetricsSnapshot;
}

function parseQueryResponse(payload: unknown): QueryResponsePayload {
  if (!isRecord(payload)) {
    throw invalidResponse("query");
  }

  const answer = payload.answer;
  const citations = payload.citations;
  const chunks = payload.chunks;
  const availability = parseQueryAvailability(payload.availability);
  const grounding = parseQueryGrounding(payload.grounding);
  const chunkIds = Array.isArray(chunks)
    ? new Set(chunks.filter(isChunkPayload).map((chunk) => chunk.chunkId))
    : new Set<string>();
  const stringCitations = Array.isArray(citations)
    ? citations.filter((citation): citation is string => typeof citation === "string")
    : [];
  const citationsAreUnique =
    Array.isArray(citations) && new Set(stringCitations).size === citations.length;
  const citationsMatchEvidence =
    Array.isArray(citations) && citations.every((citation) => chunkIds.has(String(citation)));
  const answerIsNonEmpty = typeof answer === "string" && answer.trim().length > 0;
  if (
    (answer !== null && typeof answer !== "string") ||
    !Array.isArray(citations) ||
    !citations.every((citation) => typeof citation === "string" && citation.length > 0) ||
    !Array.isArray(chunks) ||
    !chunks.every(isChunkPayload) ||
    (Array.isArray(chunks) && chunkIds.size !== chunks.length) ||
    availability === undefined ||
    grounding === undefined ||
    (grounding === "grounded" &&
      (!answerIsNonEmpty ||
        citations.length === 0 ||
        !citationsAreUnique ||
        !citationsMatchEvidence)) ||
    (grounding === "ungrounded" &&
      (answer !== null || citations.length > 0 || !citationsAreUnique)) ||
    (availability === "unavailable" &&
      (answerIsNonEmpty || citations.length > 0 || grounding !== "ungrounded"))
  ) {
    throw invalidResponse("query");
  }

  return {
    answer,
    availability,
    citations,
    chunks,
    grounding,
  };
}

function mapQueryResponse(response: QueryResponsePayload): QueryResult {
  return {
    answer:
      response.answer ??
      (response.availability === "unavailable"
        ? "The local language model is unavailable."
        : response.chunks.length > 0
        ? "The local index returned ranked sources but no grounded answer."
        : "The local index did not return evidence for this question."),
    availability: response.availability,
    citations: response.citations.map((id) => ({
      id,
      location: "Immutable chunk citation",
      source: "Local knowledge base",
      title: id,
    })),
    grounding: response.grounding,
  };
}

function parseDiagnostic(value: unknown): ReadinessDiagnostic | undefined {
  if (!isRecord(value)) {
    return undefined;
  }
  const component = value.component;
  if (
    !isHealthComponent(component) ||
    typeof value.message !== "string" ||
    typeof value.action !== "string"
  ) {
    return undefined;
  }
  return { action: value.action, component, message: value.message };
}

function isChunkPayload(value: unknown): value is QueryResponsePayload["chunks"][number] {
  return (
    isRecord(value) &&
    typeof value.chunkId === "string" &&
    isNumber(value.score) &&
    typeof value.text === "string"
  );
}

function isMetricCounts(value: unknown): value is MetricCounts {
  return isRecord(value) && Object.values(value).every(isNumber);
}

function parseDocumentCounts(
  value: unknown,
): Partial<Record<DocumentStatus, number>> | undefined {
  if (!isMetricCounts(value) || !Object.keys(value).every(isDocumentStatus)) {
    return undefined;
  }
  return value as Partial<Record<DocumentStatus, number>>;
}

function isStageCounts(value: unknown): value is Record<string, MetricCounts> {
  return isRecord(value) && Object.values(value).every(isMetricCounts);
}

function isNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0;
}

function isCount(value: unknown): value is number {
  return isNumber(value) && Number.isInteger(value);
}

function isNullableString(value: unknown): value is string | null {
  return value === null || typeof value === "string";
}

function isNullableNumber(value: unknown): value is number | null {
  return value === null || isNumber(value);
}

function isNullableScore(value: unknown): value is number | null {
  return value === null || (isNumber(value) && value <= 1);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function parseComponentStatus(value: unknown): ComponentStatus | undefined {
  return value === "available" || value === "unavailable" ? value : undefined;
}

function parseServiceStatus(value: unknown): HealthSnapshot["status"] | undefined {
  return value === "healthy" || value === "degraded" || value === "offline" ? value : undefined;
}

function parseQueryAvailability(value: unknown): QueryAvailability | undefined {
  return value === "available" || value === "unavailable" ? value : undefined;
}

function parseQueryGrounding(value: unknown): QueryGrounding | undefined {
  return value === "grounded" || value === "ungrounded" ? value : undefined;
}

function isDocumentStatus(value: unknown): value is DocumentStatus {
  return (
    value === "ARCHIVED" ||
    value === "CLEANED" ||
    value === "FAILED" ||
    value === "FAILED_QUALITY" ||
    value === "INDEXED" ||
    value === "NEW" ||
    value === "SCRAPED" ||
    value === "VECTORIZED"
  );
}

function isEntityType(value: unknown): value is EntityType {
  return (
    value === "CONCEPT" ||
    value === "EVENT" ||
    value === "LOCATION" ||
    value === "ORGANIZATION" ||
    value === "PERSON" ||
    value === "PRODUCT"
  );
}

function isJobStatus(value: unknown): value is Exclude<JobStatus, "DONE"> {
  return value === "DEAD" || value === "PENDING" || value === "RUNNING";
}

function isTopicQueueStatus(value: unknown): value is TopicQueueStatus {
  return value === "duplicate" || value === "enqueued";
}

function isHealthComponent(value: unknown): value is HealthComponent {
  return isHealthProcess(value) || isHealthProvider(value);
}

function isHealthProcess(value: unknown): value is HealthProcess {
  return (
    value === "cleaning" ||
    value === "graph" ||
    value === "indexer" ||
    value === "retrieval" ||
    value === "scraper"
  );
}

function isHealthProvider(value: unknown): value is HealthProvider {
  return (
    value === "artifactStore" ||
    value === "embeddingModel" ||
    value === "falkordb" ||
    value === "ollama" ||
    value === "qdrant"
  );
}

function invalidResponse(resource: string): Error {
  return new Error(`The local API returned an invalid ${resource} response`);
}
