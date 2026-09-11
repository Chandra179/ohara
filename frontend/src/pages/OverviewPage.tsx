import { useCallback, useEffect, useRef } from "react";
import type { DashboardSnapshot, ServiceStatus } from "../api/client";
import { useApi } from "../api/useApi";
import { QueueList } from "../components/ohara/QueueList";
import { PageHeader } from "../components/ohara/PageHeader";
import { StatCard } from "../components/ohara/StatCard";
import { Button } from "../components/ui/Button";
import { EmptyState } from "../components/ui/EmptyState";
import { ErrorState } from "../components/ui/ErrorState";
import { Panel } from "../components/ui/Panel";
import { StatusBadge, type StatusTone } from "../components/ui/StatusBadge";
import { LoadingState } from "../components/ui/LoadingState";
import { useAsyncResource } from "../hooks/useAsyncResource";
import { useNotifications } from "../notifications/useNotifications";

const SERVICE_LABELS: Record<ServiceStatus, string> = {
  degraded: "Local · Degraded",
  healthy: "Local · Healthy",
  offline: "Local · Offline",
};

const SERVICE_TONES: Record<ServiceStatus, StatusTone> = {
  degraded: "pending",
  healthy: "healthy",
  offline: "danger",
};

export function OverviewPage() {
  const api = useApi();
  const { notify } = useNotifications();
  const loadDashboard = useCallback(() => api.getDashboard(), [api]);
  const { reload, resource } = useAsyncResource<DashboardSnapshot>(loadDashboard);
  const refreshPending = useRef(false);

  const refreshDashboard = useCallback(() => {
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
        message: "Document totals and the ingestion queue are up to date.",
        title: "Ingestion status refreshed",
        tone: "success",
      });
    } else {
      notify({
        message: resource.error,
        title: "Could not refresh ingestion status",
        tone: "error",
      });
    }
  }, [notify, resource]);

  return (
    <div className="page-stack">
      <PageHeader
        actions={
          <Button
            aria-busy={resource.status === "loading"}
            disabled={resource.status === "loading"}
            onClick={refreshDashboard}
            variant="secondary"
          >
            {resource.status === "loading" ? "Refreshing…" : "Refresh"}
          </Button>
        }
        description="Your local knowledge workbench."
        label="Overview"
      />

      {resource.status === "loading" ? (
        <Panel>
          <LoadingState label="Loading workspace status" />
        </Panel>
      ) : null}

      {resource.status === "error" ? (
        <Panel>
          <ErrorState description={resource.error} onRetry={reload} />
        </Panel>
      ) : null}

      {resource.status === "success" ? <DashboardContent snapshot={resource.data} /> : null}
    </div>
  );
}

function DashboardContent({ snapshot }: { snapshot: DashboardSnapshot }) {
  return (
    <>
      <div className="metric-grid">
        <StatCard icon="file" label="Indexed" value={snapshot.indexedCount} />
        <StatCard icon="activity" label="Processing" value={snapshot.processingCount} />
        <StatCard icon="flask" label="Failed" value={snapshot.failedCount} />
      </div>

      <Panel>
        <div className="panel-heading">
          <div>
            <p className="eyebrow">Operations</p>
            <h2>Ingestion queue</h2>
          </div>
          <StatusBadge tone={SERVICE_TONES[snapshot.serviceStatus]}>
            {SERVICE_LABELS[snapshot.serviceStatus]}
          </StatusBadge>
        </div>
        {snapshot.queue.length > 0 ? (
          <QueueList items={snapshot.queue} />
        ) : (
          <EmptyState
            description="New documents will appear here while they are being processed."
            icon="activity"
            title="The ingestion queue is clear"
          />
        )}
      </Panel>
    </>
  );
}
