import { useCallback, useEffect, useRef, useState } from "react";
import type { EntityReview, MergePreview } from "../api/client";
import { useApi } from "../api/useApi";
import { EntityCard } from "../components/ohara/EntityCard";
import { PageHeader } from "../components/ohara/PageHeader";
import { Button } from "../components/ui/Button";
import { EmptyState } from "../components/ui/EmptyState";
import { ErrorState } from "../components/ui/ErrorState";
import { Icon } from "../components/ui/Icon";
import { LoadingState } from "../components/ui/LoadingState";
import { Panel } from "../components/ui/Panel";
import { StatusBadge } from "../components/ui/StatusBadge";
import { useAsyncResource } from "../hooks/useAsyncResource";
import { useNotifications } from "../notifications/useNotifications";

type PreviewState =
  | { status: "idle" }
  | { status: "loading" }
  | { error: string; status: "error" }
  | { data: MergePreview; status: "success" };

export function EntitiesPage() {
  const api = useApi();
  const { notify } = useNotifications();
  const loadReviews = useCallback(() => api.getEntityReviews(), [api]);
  const { reload, resource } = useAsyncResource<EntityReview[]>(loadReviews);
  const refreshPending = useRef(false);
  const [selectedId, setSelectedId] = useState<string>();
  const [preview, setPreview] = useState<PreviewState>({ status: "idle" });

  const refreshReviews = useCallback(() => {
    refreshPending.current = true;
    reload();
  }, [reload]);

  useEffect(() => {
    if (!refreshPending.current || resource.status === "loading") {
      return;
    }

    refreshPending.current = false;
    if (resource.status === "success") {
      notify({
        message: "Pending entity candidates are now up to date.",
        title: "Entity reviews refreshed",
        tone: "success",
      });
    } else {
      notify({
        message: resource.error,
        title: "Could not refresh entity reviews",
        tone: "error",
      });
    }
  }, [notify, resource]);

  const selectReview = useCallback(
    async (reviewId: string) => {
      setSelectedId(reviewId);
      setPreview({ status: "loading" });

      try {
        const data = await api.previewEntityMerge(reviewId);
        setPreview({ data, status: "success" });
      } catch (error: unknown) {
        const message = errorMessage(error);
        setPreview({ error: message, status: "error" });
        notify({ message, title: "Merge preview failed", tone: "error" });
      }
    },
    [api, notify],
  );

  return (
    <div className="page-stack">
      <PageHeader
        actions={
          <Button
            aria-busy={resource.status === "loading"}
            disabled={resource.status === "loading"}
            onClick={refreshReviews}
            variant="secondary"
          >
            {resource.status === "loading" ? "Refreshing…" : "Refresh"}
          </Button>
        }
        description="Review and resolve people, places, and concepts."
        label="Entities"
      />

      {resource.status === "loading" ? (
        <Panel>
          <LoadingState label="Loading entity review" />
        </Panel>
      ) : null}

      {resource.status === "error" ? (
        <Panel>
          <ErrorState description={resource.error} onRetry={reload} />
        </Panel>
      ) : null}

      {resource.status === "success" && resource.data.length === 0 ? (
        <Panel>
          <EmptyState
            description="New candidates will appear here when the knowledge graph finds possible duplicates."
            icon="nodes"
            title="No entities need review"
          />
        </Panel>
      ) : null}

      {resource.status === "success" && resource.data.length > 0 ? (
        <ReviewWorkspace
          onClear={() => {
            setSelectedId(undefined);
            setPreview({ status: "idle" });
          }}
          onSelect={selectReview}
          preview={preview}
          reviews={resource.data}
          selectedId={selectedId}
        />
      ) : null}
    </div>
  );
}

interface ReviewWorkspaceProps {
  onClear: () => void;
  onSelect: (reviewId: string) => void;
  preview: PreviewState;
  reviews: EntityReview[];
  selectedId: string | undefined;
}

function ReviewWorkspace({
  onClear,
  onSelect,
  preview,
  reviews,
  selectedId,
}: ReviewWorkspaceProps) {
  return (
    <div className="entities-layout">
      <Panel className="review-panel">
        <div className="panel-heading">
          <div>
            <p className="eyebrow">Entity resolution</p>
            <h2>Pending review</h2>
          </div>
          <StatusBadge tone="pending">{reviews.length} candidates</StatusBadge>
        </div>

        <div className="review-list">
          {reviews.map((review) => (
            <button
              aria-pressed={selectedId === review.id}
              className={`review-row${selectedId === review.id ? " review-row--selected" : ""}`}
              key={review.id}
              onClick={() => onSelect(review.id)}
              type="button"
            >
              <Icon name="nodes" />
              <span className="review-row__copy">
                <strong>
                  {review.candidateA.name} ↔ {review.candidateB.name}
                </strong>
                <small>{scoreLabel(review.score)}</small>
              </span>
              <span className="review-row__matches">{review.candidateA.type}</span>
            </button>
          ))}
        </div>
      </Panel>

      <Panel className="merge-panel">
        <div className="panel-heading">
          <div>
            <p className="eyebrow">Identity review</p>
            <h2>Candidate preview</h2>
          </div>
          {preview.status === "success" ? <StatusBadge tone="healthy">Ready</StatusBadge> : null}
        </div>

        {preview.status === "idle" ? (
          <EmptyState
            description="Select a pending candidate to inspect the similarity evidence."
            icon="nodes"
            title="Select a candidate"
          />
        ) : null}
        {preview.status === "loading" ? <LoadingState label="Preparing merge preview" /> : null}
        {preview.status === "error" ? (
          <ErrorState
            description={preview.error}
            onRetry={selectedId ? () => void onSelect(selectedId) : undefined}
          />
        ) : null}
        {preview.status === "success" ? (
          <>
            <div className="entity-merge-flow">
              <EntityCard entity={preview.data.candidateA} label="Candidate A" />
              <span aria-hidden="true" className="merge-arrow">
                ↔
              </span>
              <EntityCard entity={preview.data.candidateB} label="Candidate B" />
            </div>
            <div className="result-entity">
              <span className="entity-card__label">Similarity score</span>
              <strong>{scoreLabel(preview.data.score)}</strong>
              <small>Review the candidates before taking an offline merge action.</small>
            </div>
            <div className="merge-actions">
              <Button onClick={onClear} variant="secondary">
                Close preview
              </Button>
            </div>
          </>
        ) : null}
      </Panel>
    </div>
  );
}

function scoreLabel(score: number | null): string {
  return score === null ? "No score recorded" : `${(score * 100).toFixed(1)}% match`;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : "The entity operation could not be completed";
}
