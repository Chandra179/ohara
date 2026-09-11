import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { DocumentRecord, DocumentType, QueueStatus } from "../api/client";
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
import { StatusText } from "../components/ohara/StatusText";
import { PageHeader } from "../components/ohara/PageHeader";
import { useAsyncResource } from "../hooks/useAsyncResource";
import { useNotifications } from "../notifications/useNotifications";

const PAGE_SIZE = 6;

export function DocumentsPage() {
  const api = useApi();
  const { notify } = useNotifications();
  const loadDocuments = useCallback(() => api.listDocuments(), [api]);
  const { reload, resource } = useAsyncResource(loadDocuments);
  const refreshPending = useRef(false);
  const [search, setSearch] = useState("");
  const [status, setStatus] = useState<QueueStatus | "all">("all");
  const [type, setType] = useState<DocumentType | "all">("all");
  const [source, setSource] = useState("all");
  const [page, setPage] = useState(1);

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

  const documents = resource.status === "success" ? resource.data : undefined;
  const filteredDocuments = useMemo(() => {
    if (!documents) {
      return [];
    }

    const normalizedSearch = search.trim().toLowerCase();

    return documents.filter((document) => {
      const matchesSearch =
        normalizedSearch.length === 0 ||
        document.name.toLowerCase().includes(normalizedSearch) ||
        document.source.toLowerCase().includes(normalizedSearch);
      const matchesStatus = status === "all" || document.status === status;
      const matchesType = type === "all" || document.type === type;
      const matchesSource = source === "all" || document.source === source;

      return matchesSearch && matchesStatus && matchesType && matchesSource;
    });
  }, [documents, search, source, status, type]);

  useEffect(() => {
    setPage(1);
  }, [search, source, status, type]);

  const totalPages = Math.max(1, Math.ceil(filteredDocuments.length / PAGE_SIZE));
  const currentPage = Math.min(page, totalPages);
  const visibleDocuments = filteredDocuments.slice(
    (currentPage - 1) * PAGE_SIZE,
    currentPage * PAGE_SIZE,
  );

  const columns: TableColumn<DocumentRecord>[] = [
    {
      header: "Name",
      key: "name",
      render: (document) => (
        <span className="document-name">
          <Icon name="file" />
          {document.name}
        </span>
      ),
    },
    { header: "Type", key: "type", render: (document) => document.type },
    { header: "Source", key: "source", render: (document) => document.source },
    { header: "Status", key: "status", render: (document) => <StatusText status={document.status} /> },
    { header: "Updated", key: "updatedAt", render: (document) => document.updatedAt },
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
        description="Browse and manage your local knowledge."
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
              onChange={(event) => setSearch(event.target.value)}
              placeholder="Search by name or source"
              value={search}
            />
            <Select
              label="Status"
              onChange={(event) => setStatus(event.target.value as QueueStatus | "all")}
              options={[
                { label: "All statuses", value: "all" },
                { label: "Indexed", value: "indexed" },
                { label: "Processing", value: "processing" },
                { label: "Failed", value: "failed" },
              ]}
              value={status}
            />
            <Select
              label="Type"
              onChange={(event) => setType(event.target.value as DocumentType | "all")}
              options={[
                { label: "All types", value: "all" },
                { label: "PDF", value: "PDF" },
                { label: "DOCX", value: "DOCX" },
                { label: "Markdown", value: "MD" },
                { label: "Text", value: "TXT" },
              ]}
              value={type}
            />
            <Select
              label="Source"
              onChange={(event) => setSource(event.target.value)}
              options={[{ label: "All sources", value: "all" }, { label: "Local", value: "Local" }]}
              value={source}
            />
          </div>

          <div className="panel-heading panel-heading--compact">
            <div>
              <p className="eyebrow">Knowledge base</p>
              <h2>
                {filteredDocuments.length} {filteredDocuments.length === 1 ? "document" : "documents"}
              </h2>
            </div>
            <span className="muted-copy">
              Page {currentPage} of {totalPages}
            </span>
          </div>

          {visibleDocuments.length > 0 ? (
            <Table
              caption="Documents in the local knowledge base"
              columns={columns}
              getRowKey={(document) => document.id}
              rows={visibleDocuments}
            />
          ) : (
            <EmptyState
              description="Try a different search term or remove one of the filters."
              icon="search"
              title="No documents match"
            />
          )}

          <div className="table-footer">
            <span className="muted-copy">
              Showing {visibleDocuments.length} of {filteredDocuments.length}
            </span>
            <div className="pagination-actions">
              <Button
                disabled={currentPage === 1}
                onClick={() => setPage((current) => Math.max(1, current - 1))}
                variant="secondary"
              >
                Previous
              </Button>
              <Button
                disabled={currentPage === totalPages}
                onClick={() => setPage((current) => Math.min(totalPages, current + 1))}
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
