import type { MetricCounts } from "../../api/client";
import { EmptyState } from "../ui/EmptyState";
import { Table, type TableColumn } from "../ui/Table";

const JOB_STATUSES = ["PENDING", "RUNNING", "DONE", "DEAD"] as const;

interface StageOutcomeRow {
  dead: number;
  done: number;
  pending: number;
  running: number;
  stage: string;
  total: number;
}

interface StageOutcomeTableProps {
  jobsByStageStatus: Record<string, MetricCounts>;
}

export function StageOutcomeTable({ jobsByStageStatus }: StageOutcomeTableProps) {
  const rows = Object.entries(jobsByStageStatus)
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([stage, statuses]) => ({
      dead: countStatus(statuses, "DEAD"),
      done: countStatus(statuses, "DONE"),
      pending: countStatus(statuses, "PENDING"),
      running: countStatus(statuses, "RUNNING"),
      stage,
      total: JOB_STATUSES.reduce((total, status) => total + countStatus(statuses, status), 0),
    }));

  if (rows.length === 0) {
    return (
      <EmptyState
        description="The metrics service has not reported any pipeline stages yet."
        icon="activity"
        title="No stage activity"
      />
    );
  }

  const columns: TableColumn<StageOutcomeRow>[] = [
    { header: "Stage", key: "stage", render: (row) => formatStage(row.stage) },
    { header: "Pending", key: "pending", render: (row) => row.pending.toLocaleString() },
    { header: "Running", key: "running", render: (row) => row.running.toLocaleString() },
    { header: "Completed", key: "done", render: (row) => row.done.toLocaleString() },
    { header: "Dead", key: "dead", render: (row) => row.dead.toLocaleString() },
    { header: "Total", key: "total", render: (row) => row.total.toLocaleString() },
  ];

  return (
    <Table
      caption="Pipeline jobs grouped by stage and status"
      columns={columns}
      getRowKey={(row) => row.stage}
      rows={rows}
    />
  );
}

function countStatus(statuses: MetricCounts, status: string): number {
  return statuses[status] ?? 0;
}

function formatStage(stage: string): string {
  return stage
    .toLowerCase()
    .replaceAll("_", " ")
    .replace(/(^|\s)\S/g, (letter) => letter.toUpperCase());
}
