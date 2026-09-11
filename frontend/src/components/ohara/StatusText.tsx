import { StatusBadge, type StatusTone } from "../ui/StatusBadge";
import type { QueueStatus } from "../../api/client";

const STATUS_LABELS: Record<QueueStatus, string> = {
  failed: "Failed",
  indexed: "Indexed",
  processing: "Processing",
};

const STATUS_TONES: Record<QueueStatus, StatusTone> = {
  failed: "danger",
  indexed: "healthy",
  processing: "pending",
};

export function StatusText({ status }: { status: QueueStatus }) {
  return (
    <StatusBadge tone={STATUS_TONES[status]}>
      {STATUS_LABELS[status]}
    </StatusBadge>
  );
}
