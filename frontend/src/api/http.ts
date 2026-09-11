import type {
  HealthSnapshot,
  MetricsSnapshot,
  OharaApi,
  QueryResult,
} from "./client";

interface HttpApiOptions {
  baseUrl?: string;
  fetcher?: typeof fetch;
}

interface QueryResponsePayload {
  answer: string | null;
  citations: string[];
  chunks: Array<{ chunkId: string; score: number; text: string }>;
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
    getDashboard: () => unsupported("dashboard"),
    getEntityReviews: () => unsupported("entity reviews"),
    getHealth: () => request<HealthSnapshot>("/api/health"),
    getMetrics: () => request<MetricsSnapshot>("/api/metrics"),
    listDocuments: () => unsupported("documents"),
    mergeEntities: () => unsupported("entity merge"),
    previewEntityMerge: () => unsupported("entity merge preview"),
    queryKnowledgeBase: async (question) => {
      const response = await request<QueryResponsePayload>("/api/query", {
        body: JSON.stringify({ query: question }),
        headers: { "content-type": "application/json" },
        method: "POST",
      });
      return mapQueryResponse(response);
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

function mapQueryResponse(response: QueryResponsePayload): QueryResult {
  const grounded = response.answer !== null && response.citations.length > 0;
  return {
    answer:
      response.answer ??
      (response.chunks.length > 0
        ? "The local index returned ranked sources but no grounded answer."
        : "The local index did not return evidence for this question."),
    availability: "available",
    citations: response.citations.map((id) => ({
      id,
      location: "Immutable chunk citation",
      source: "Local knowledge base",
      title: id,
    })),
    grounding: grounded ? "grounded" : "ungrounded",
  };
}

function unsupported<T>(resource: string): Promise<T> {
  return Promise.reject(new ApiRequestError(`The ${resource} API is not available yet`, 501));
}
