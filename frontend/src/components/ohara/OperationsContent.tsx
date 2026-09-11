import type { LlmUsageSnapshot, MetricCounts, MetricsSnapshot } from "../../api/client";
import { Panel } from "../ui/Panel";
import { StatCard } from "./StatCard";
import { StageOutcomeTable } from "./StageOutcomeTable";

interface OperationsContentProps {
  snapshot: MetricsSnapshot;
}

export function OperationsContent({ snapshot }: OperationsContentProps) {
  const pendingJobs = countJobs(snapshot.jobsByStageStatus, "PENDING");
  const runningJobs = countJobs(snapshot.jobsByStageStatus, "RUNNING");
  const deadJobs = countJobs(snapshot.jobsByStageStatus, "DEAD");

  return (
    <>
      <div className="metric-grid operations-summary-grid">
        <StatCard
          caption="jobs"
          icon="activity"
          label="Active queue"
          value={pendingJobs + runningJobs}
        />
        <StatCard caption="jobs" icon="flask" label="Dead letter" value={deadJobs} />
        <StatCard caption="reviews" icon="nodes" label="Pending ER" value={snapshot.pendingErReviews} />
        <StatCard
          caption="documents"
          icon="file"
          label="Due for recrawl"
          value={snapshot.dueForRecrawl}
        />
      </div>

      <Panel>
        <div className="panel-heading">
          <div>
            <p className="eyebrow">Queue health</p>
            <h2>Pipeline activity</h2>
          </div>
          <span className="muted-copy">{formatCapturedAt(snapshot.capturedAt)}</span>
        </div>
        <StageOutcomeTable jobsByStageStatus={snapshot.jobsByStageStatus} />
      </Panel>

      <div className="operations-grid">
        <Panel>
          <div className="panel-heading panel-heading--compact">
            <div>
              <p className="eyebrow">Audit trail</p>
              <h2>Outcomes</h2>
            </div>
          </div>
          <MetricBreakdown entries={snapshot.eventsByOutcome} emptyLabel="No audit outcomes recorded" />
        </Panel>

        <Panel>
          <div className="panel-heading panel-heading--compact">
            <div>
              <p className="eyebrow">Audit trail</p>
              <h2>By stage</h2>
            </div>
          </div>
          <MetricBreakdown entries={snapshot.eventsByStage} emptyLabel="No stage events recorded" />
        </Panel>
      </div>

      <div className="operations-grid">
        <Panel>
          <div className="panel-heading panel-heading--compact">
            <div>
              <p className="eyebrow">Usage ledger</p>
              <h2>LLM usage</h2>
            </div>
          </div>
          <UsageSummary usage={snapshot.llmUsage} />
        </Panel>

        <Panel>
          <div className="panel-heading panel-heading--compact">
            <div>
              <p className="eyebrow">Local storage</p>
              <h2>Raw payloads</h2>
            </div>
          </div>
          <StorageSummary snapshot={snapshot} />
        </Panel>
      </div>

      <Panel>
        <div className="notice notice--pending">
          <strong>Throughput and latency are not available yet</strong>
          <span>
            The Operations view reports durable queue and usage metrics only. Charts will appear after
            the backend exposes stage timing counters.
          </span>
        </div>
      </Panel>
    </>
  );
}

function MetricBreakdown({ entries, emptyLabel }: { entries: MetricCounts; emptyLabel: string }) {
  const rows = Object.entries(entries).sort(([left], [right]) => left.localeCompare(right));

  if (rows.length === 0) {
    return <p className="muted-copy">{emptyLabel}</p>;
  }

  return (
    <dl className="metric-list">
      {rows.map(([label, value]) => (
        <div className="metric-list__row" key={label}>
          <dt>{formatLabel(label)}</dt>
          <dd>{value.toLocaleString()}</dd>
        </div>
      ))}
    </dl>
  );
}

function UsageSummary({ usage }: { usage: LlmUsageSnapshot }) {
  const successRate = usage.calls > 0 ? `${Math.round((usage.successfulCalls / usage.calls) * 100)}%` : "—";

  return (
    <dl className="metric-list">
      <MetricRow label="Completion attempts" value={usage.calls.toLocaleString()} />
      <MetricRow label="Successful" value={`${usage.successfulCalls.toLocaleString()} (${successRate})`} />
      <MetricRow label="Failed" value={usage.failedCalls.toLocaleString()} />
      <MetricRow label="Prompt tokens" value={usage.promptTokens.toLocaleString()} />
      <MetricRow label="Completion tokens" value={usage.completionTokens.toLocaleString()} />
      <MetricRow label="Estimated cost" value={formatCost(usage.estimatedCostMicros)} />
    </dl>
  );
}

function StorageSummary({ snapshot }: { snapshot: MetricsSnapshot }) {
  return (
    <dl className="metric-list">
      <MetricRow label="Files" value={snapshot.rawFiles.toLocaleString()} />
      <MetricRow label="Used" value={formatBytes(snapshot.rawBytes)} />
      <MetricRow label="Byte limit" value={formatOptionalBytes(snapshot.rawMaxBytes)} />
      <MetricRow
        label="Age limit"
        value={snapshot.rawMaxAgeDays === null ? "Not configured" : `${snapshot.rawMaxAgeDays} days`}
      />
    </dl>
  );
}

function MetricRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="metric-list__row">
      <dt>{label}</dt>
      <dd>{value}</dd>
    </div>
  );
}

function countJobs(jobsByStageStatus: Record<string, MetricCounts>, status: string): number {
  return Object.values(jobsByStageStatus).reduce((total, statuses) => total + (statuses[status] ?? 0), 0);
}

function formatCapturedAt(value: string): string {
  const timestamp = new Date(value);
  if (Number.isNaN(timestamp.getTime())) {
    return `Captured ${value}`;
  }

  return `Captured ${timestamp.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" })}`;
}

function formatLabel(value: string): string {
  return value
    .toLowerCase()
    .replaceAll("_", " ")
    .replace(/(^|\s)\S/g, (letter) => letter.toUpperCase());
}

function formatCost(micros: number): string {
  return (micros / 1_000_000).toLocaleString(undefined, {
    currency: "USD",
    maximumFractionDigits: 6,
    minimumFractionDigits: 2,
    style: "currency",
  });
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) {
    return `${bytes.toLocaleString()} B`;
  }

  const units = ["KiB", "MiB", "GiB"];
  let value = bytes;
  let unitIndex = -1;
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex += 1;
  }

  return `${value.toLocaleString(undefined, { maximumFractionDigits: 1 })} ${units[unitIndex]}`;
}

function formatOptionalBytes(bytes: number | null): string {
  return bytes === null ? "Not configured" : formatBytes(bytes);
}
