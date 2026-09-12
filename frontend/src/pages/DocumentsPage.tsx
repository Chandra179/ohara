import { useCallback, useEffect, useRef, useState } from "react";
import type { DocumentRecord, DocumentStatus } from "../api/client";
import { useApi } from "../api/useApi";
import { Icon } from "../components/ui/Icon";
import { Button } from "../components/ui/Button";
import { EmptyState } from "../components/ui/EmptyState";
import { ErrorState } from "../components/ui/ErrorState";
import { Input } from "../components/ui/Input";
import { LoadingState } from "../components/ui/LoadingState";
import { Panel } from "../components/ui/Panel";
import { Select } from "../components/ui/Select";
import { Table, type TableColumn } from "../components/ui/Table";
import { DocumentStatusText } from "../components/ohara/StatusText";
import { PageHeader } from "../components/ohara/PageHeader";
import { useAsyncResource } from "../hooks/useAsyncResource";
import { useNotifications } from "../notifications/useNotifications";

const PAGE_SIZE = 25;

export function DocumentsPage() {
  const api = useApi();
  const { notify } = useNotifications();
  const [search, setSearch] = useState("");
  const [status, setStatus] = useState<DocumentStatus | "all">("all");
  const [page, setPage] = useState(1);
  const [pageCursors, setPageCursors] = useState<Array<string | undefined>>([undefined]);
  const currentCursor = pageCursors[page - 1];
  const loadDocuments = useCallback(
    () =>
      api.listDocuments({
        cursor: currentCursor,
        limit: PAGE_SIZE,
        search: search.trim() || undefined,
        status: status === "all" ? undefined : status,
      }),
    [api, currentCursor, search, status],
  );
  const { reload, resource } = useAsyncResource(loadDocuments);
  const refreshPending = useRef(false);

  const refreshDocuments = useCallback(() => {
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
        message: "The latest document statuses are now displayed.",
        title: "Documents refreshed",
        tone: "success",
      });
    } else {
      notify({
        message: resource.error,
        title: "Could not refresh documents",
        tone: "error",
      });
    }
  }, [notify, resource]);

  const documents = resource.status === "success" ? resource.data.items : [];

  const resetPagination = useCallback(() => {
    setPage(1);
    setPageCursors([undefined]);
  }, []);

  const goToNextPage = useCallback(() => {
    if (resource.status !== "success" || resource.data.nextCursor === null) {
      return;
    }
    setPageCursors((current) => [...current, resource.data.nextCursor ?? undefined]);
    setPage((current) => current + 1);
  }, [resource]);

  const goToPreviousPage = useCallback(() => {
    setPage((current) => Math.max(1, current - 1));
  }, []);

  const columns: TableColumn<DocumentRecord>[] = [
    {
      header: "Name",
      key: "name",
      render: (document) => (
        <div className="document-name">
          <Icon name="file" />
          <span>
            <strong>{document.title ?? document.sourceUrl}</strong>
            {document.title ? <small>{document.sourceUrl}</small> : null}
          </span>
        </div>
      ),
    },
    {
      header: "Status",
      key: "status",
      render: (document) => <DocumentStatusText status={document.status} />,
    },
    { header: "Chunks", key: "chunkCount", render: (document) => document.chunkCount },
    {
      header: "Updated",
      key: "updatedAt",
      render: (document) => document.lastProcessedAt ?? document.createdAt,
    },
  ];

  return (
    <div className="page-stack">
      <PageHeader
        actions={
          <Button
            aria-busy={resource.status === "loading"}
            disabled={resource.status === "loading"}
            onClick={refreshDocuments}
            variant="secondary"
          >
            {resource.status === "loading" ? "Refreshing…" : "Refresh"}
          </Button>
        }
        description="Browse the durable documents registered in your local knowledge base."
        label="Documents"
      />

      {resource.status === "loading" ? (
        <Panel>
          <LoadingState label="Loading documents" />
        </Panel>
      ) : null}

      {resource.status === "error" ? (
        <Panel>
          <ErrorState description={resource.error} onRetry={reload} />
        </Panel>
      ) : null}

      {resource.status === "success" ? (
        <Panel>
          <div className="filters" aria-label="Document filters">
            <Input
              label="Search documents"
              onChange={(event) => {
                setSearch(event.target.value);
                resetPagination();
              }}
              placeholder="Search by title or source URL"
              value={search}
            />
            <Select
              label="Status"
              onChange={(event) => {
                setStatus(event.target.value as DocumentStatus | "all");
                resetPagination();
              }}
              options={[
                { label: "All statuses", value: "all" },
                { label: "New", value: "NEW" },
                { label: "Scraped", value: "SCRAPED" },
                { label: "Cleaned", value: "CLEANED" },
                { label: "Vectorized", value: "VECTORIZED" },
                { label: "Indexed", value: "INDEXED" },
                { label: "Failed quality", value: "FAILED_QUALITY" },
                { label: "Failed", value: "FAILED" },
                { label: "Archived", value: "ARCHIVED" },
              ]}
              value={status}
            />
          </div>

          <div className="panel-heading panel-heading--compact">
            <div>
              <p className="eyebrow">Knowledge base</p>
              <h2>
                {documents.length} {documents.length === 1 ? "document" : "documents"}
              </h2>
            </div>
            <span className="muted-copy">
              Page {page}
            </span>
          </div>

          {documents.length > 0 ? (
            <Table
              caption="Documents in the local knowledge base"
              columns={columns}
              getRowKey={(document) => document.id}
              rows={documents}
            />
          ) : (
            <EmptyState
              description="Register a document or adjust the current search and status filter."
              icon="search"
              title={search.length > 0 || status !== "all" ? "No documents match" : "No documents yet"}
            />
          )}

          <div className="table-footer">
            <span className="muted-copy">
              Showing {documents.length} on this page
            </span>
            <div className="pagination-actions">
              <Button
                disabled={page === 1}
                onClick={goToPreviousPage}
                variant="secondary"
              >
                Previous
              </Button>
              <Button
                disabled={resource.status !== "success" || resource.data.nextCursor === null}
                onClick={goToNextPage}
                variant="secondary"
              >
                Next
              </Button>
            </div>
          </div>
        </Panel>
      ) : null}
    </div>
  );
}
