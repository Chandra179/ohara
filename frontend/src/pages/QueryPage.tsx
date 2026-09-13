import { useCallback, useState, type FormEvent } from "react";
import type { QueryResult } from "../api/client";
import { useApi } from "../api/useApi";
import { CitationCard } from "../components/ohara/CitationCard";
import { PageHeader } from "../components/ohara/PageHeader";
import { Button } from "../components/ui/Button";
import { EmptyState } from "../components/ui/EmptyState";
import { ErrorState } from "../components/ui/ErrorState";
import { LoadingState } from "../components/ui/LoadingState";
import { Panel } from "../components/ui/Panel";
import { useNotifications } from "../notifications/useNotifications";

type QueryState =
  | { status: "error"; error: string }
  | { status: "idle" }
  | { status: "loading" }
  | { data: QueryResult; status: "success" };

export function QueryPage() {
  const api = useApi();
  const { notify } = useNotifications();
  const [question, setQuestion] = useState("");
  const [validationError, setValidationError] = useState<string | undefined>();
  const [queryState, setQueryState] = useState<QueryState>({ status: "idle" });

  const submitQuestion = useCallback(
    async (value = question) => {
      const normalizedQuestion = value.trim();
      if (normalizedQuestion.length === 0) {
        setValidationError("Enter a question to search your knowledge base");
        return;
      }

      setValidationError(undefined);
      setQueryState({ status: "loading" });

      try {
        const data = await api.queryKnowledgeBase(normalizedQuestion);
        setQueryState({ data, status: "success" });
        notify({
          message:
            data.availability === "unavailable"
              ? "Start the configured local model before asking for a grounded answer."
              : data.grounding === "ungrounded"
                ? "The index did not provide enough evidence for a cited answer."
                : `${data.citations.length} sources support the answer.`,
          title:
            data.availability === "unavailable"
              ? "Query completed without a model"
              : data.grounding === "ungrounded"
                ? "Query completed without grounded evidence"
                : "Query completed",
          tone:
            data.availability === "unavailable" || data.grounding === "ungrounded"
              ? "info"
              : "success",
        });
      } catch (error: unknown) {
        const message = errorMessage(error);
        setQueryState({ error: message, status: "error" });
        notify({ message, title: "Query failed", tone: "error" });
      }
    },
    [api, notify, question],
  );

  function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    void submitQuestion();
  }

  return (
    <div className="page-stack">
      <PageHeader description="Ask questions across your local knowledge." label="Query" />

      <Panel>
        <form className="query-form" onSubmit={handleSubmit}>
          <label className="query-form__label" htmlFor="knowledge-query">
            Ask your knowledge base
          </label>
          <div className="query-form__row">
            <input
              aria-describedby={validationError ? "knowledge-query-error" : undefined}
              aria-invalid={Boolean(validationError)}
              className="query-input"
              id="knowledge-query"
              onChange={(event) => {
                setQuestion(event.target.value);
                setValidationError(undefined);
              }}
              placeholder="Ask your knowledge base…"
              value={question}
            />
            <Button disabled={queryState.status === "loading"} type="submit">
              {queryState.status === "loading" ? "Asking…" : "Ask"}
            </Button>
          </div>
          {validationError ? (
            <span className="field__error" id="knowledge-query-error" role="alert">
              {validationError}
            </span>
          ) : null}
        </form>
      </Panel>

      {queryState.status === "idle" ? (
        <Panel>
          <EmptyState
            description="Search across indexed documents and connected entities, with citations for every grounded answer."
            icon="search"
            title="Start with a question"
          />
        </Panel>
      ) : null}

      {queryState.status === "loading" ? (
        <Panel>
          <LoadingState label="Searching your local knowledge" />
        </Panel>
      ) : null}

      {queryState.status === "error" ? (
        <Panel>
          <ErrorState description={queryState.error} onRetry={() => void submitQuestion()} />
        </Panel>
      ) : null}

      {queryState.status === "success" ? (
        <QueryResultView onRetry={() => void submitQuestion()} result={queryState.data} />
      ) : null}
    </div>
  );
}

function QueryResultView({ onRetry, result }: { onRetry: () => void; result: QueryResult }) {
  if (result.availability === "unavailable") {
    return (
      <Panel>
        <ErrorState
          description="The local language model is unavailable. Start the configured provider and try again."
          onRetry={onRetry}
          title="Local model unavailable"
        />
      </Panel>
    );
  }

  if (result.grounding === "ungrounded") {
    return (
      <Panel>
        <div className="notice notice--pending" role="status">
          <strong>No grounded answer</strong>
          <span>{result.answer}</span>
        </div>
      </Panel>
    );
  }

  return (
    <>
      <Panel>
        <div className="answer-heading">
          <div>
            <p className="eyebrow">Response</p>
            <h2>Answer</h2>
          </div>
          <span className="muted-copy">Just now · Semantic retrieval</span>
        </div>
        <p className="answer-copy">{result.answer}</p>
      </Panel>

      <Panel>
        <div className="answer-heading">
          <div>
            <p className="eyebrow">Evidence</p>
            <h2>Citations</h2>
          </div>
          <span className="muted-copy">{result.citations.length} sources</span>
        </div>
        <div className="citations-grid">
          {result.citations.map((citation) => (
            <CitationCard citation={citation} key={citation.id} />
          ))}
        </div>
      </Panel>
    </>
  );
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : "The query could not be completed";
}
