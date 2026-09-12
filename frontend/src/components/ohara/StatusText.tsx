import { StatusBadge, type StatusTone } from "../ui/StatusBadge";
import type { DocumentStatus, JobStatus } from "../../api/client";

const DOCUMENT_STATUS_LABELS: Record<DocumentStatus, string> = {
  ARCHIVED: "Archived",
  CLEANED: "Cleaned",
  FAILED: "Failed",
  FAILED_QUALITY: "Failed quality",
  INDEXED: "Indexed",
  NEW: "New",
  SCRAPED: "Scraped",
  VECTORIZED: "Vectorized",
};

const DOCUMENT_STATUS_TONES: Record<DocumentStatus, StatusTone> = {
  ARCHIVED: "muted",
  CLEANED: "pending",
  FAILED: "danger",
  FAILED_QUALITY: "danger",
  INDEXED: "healthy",
  NEW: "pending",
  SCRAPED: "pending",
  VECTORIZED: "pending",
};

const JOB_STATUS_LABELS: Record<Exclude<JobStatus, "DONE">, string> = {
  DEAD: "Failed",
  PENDING: "Pending",
  RUNNING: "Running",
};

const JOB_STATUS_TONES: Record<Exclude<JobStatus, "DONE">, StatusTone> = {
  DEAD: "danger",
  PENDING: "pending",
  RUNNING: "healthy",
};

export function DocumentStatusText({ status }: { status: DocumentStatus }) {
  return (
    <StatusBadge tone={DOCUMENT_STATUS_TONES[status]}>
      {DOCUMENT_STATUS_LABELS[status]}
    </StatusBadge>
  );
}

export function JobStatusText({ status }: { status: Exclude<JobStatus, "DONE"> }) {
  return (
    <StatusBadge tone={JOB_STATUS_TONES[status]}>
      {JOB_STATUS_LABELS[status]}
    </StatusBadge>
  );
}
