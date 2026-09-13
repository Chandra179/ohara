import { useCallback, useEffect, useRef } from "react";
import type { MetricsSnapshot } from "../api/client";
import { useApi } from "../api/useApi";
import { OperationsContent } from "../components/ohara/OperationsContent";
import { PageHeader } from "../components/ohara/PageHeader";
import { Button } from "../components/ui/Button";
import { ErrorState } from "../components/ui/ErrorState";
import { LoadingState } from "../components/ui/LoadingState";
import { Panel } from "../components/ui/Panel";
import { useAsyncResource } from "../hooks/useAsyncResource";
import { useNotifications } from "../notifications/useNotifications";

export function OperationsPage() {
  const api = useApi();
  const { notify } = useNotifications();
  const loadMetrics = useCallback(() => api.getMetrics(), [api]);
  const { reload, resource } = useAsyncResource<MetricsSnapshot>(loadMetrics);
  const refreshPending = useRef(false);

  const refreshMetrics = useCallback(() => {
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
        message: "The latest queue, audit, and usage totals are now displayed.",
        title: "Operations refreshed",
        tone: "success",
      });
    } else {
      notify({
        message: resource.error,
        title: "Could not refresh operations",
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
            onClick={refreshMetrics}
            variant="secondary"
          >
            {resource.status === "loading" ? "Refreshing…" : "Refresh"}
          </Button>
        }
        description="Inspect process health and operational activity."
        label="Operations"
      />

      {resource.status === "loading" ? (
        <Panel>
          <LoadingState label="Loading operational metrics" />
        </Panel>
      ) : null}

      {resource.status === "error" ? (
        <Panel>
          <ErrorState description={resource.error} onRetry={reload} />
        </Panel>
      ) : null}

      {resource.status === "success" ? <OperationsContent snapshot={resource.data} /> : null}
    </div>
  );
}
