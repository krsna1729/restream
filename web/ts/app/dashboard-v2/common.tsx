import type {
  OverviewMetric,
  OverviewStatus,
  OverviewTone,
  OverviewViewModel,
} from "../../features/overview-view-model.js";
import type {
  PipelineOutputCardModel,
  PipelineOutputOverviewModel,
} from "../../features/pipeline-operate-view-model.js";

// ── Tone / colour constants ──────────────────────────────────────────

export const toneClasses: Readonly<Record<OverviewTone, string>> = {
  success: "border-success/30 bg-success/10 text-success",
  warning: "border-warning/35 bg-warning/10 text-warning",
  error: "border-error/35 bg-error/10 text-error",
  info: "border-info/30 bg-info/10 text-info",
  neutral: "border-base-content/10 bg-base-100/80 text-base-content/75",
};

export const toneTextClasses: Readonly<Record<OverviewTone, string>> = {
  success: "text-success",
  warning: "text-warning",
  error: "text-error",
  info: "text-info",
  neutral: "text-base-content/75",
};

export const INPUT_AUDIO_TRACK_PREVIEW_LIMIT = 6;

export const metricToneClasses: Readonly<
  Record<OverviewMetric["key"], string>
> = {
  inputs: "border-l-success text-success",
  outputs: "border-l-secondary text-secondary",
  inputKbps: "border-l-accent text-accent",
  outputKbps: "border-l-primary text-primary",
  engineCpu: "border-l-warning text-warning",
  engineMemory: "border-l-info text-info",
};

// ── Generic UI components ────────────────────────────────────────────

export function Panel({
  children,
  className = "",
  labelledBy,
}: {
  children: React.ReactNode;
  className?: string;
  labelledBy?: string;
}): React.JSX.Element {
  return (
    <section
      aria-labelledby={labelledBy}
      className={`dashboard-section ${className}`}
    >
      {children}
    </section>
  );
}

export function StatusBadge({
  showDetail = true,
  status,
}: {
  showDetail?: boolean;
  status: OverviewStatus;
}): React.JSX.Element {
  return (
    <span
      className={`${toneClasses[status.tone]} inline-flex min-h-8 max-w-full items-center gap-2 rounded-lg border px-2.5 py-1 text-xs font-semibold leading-tight`}
    >
      <span className="truncate">{status.label}</span>
      {showDetail && status.detail ? (
        <span className="text-base-content font-normal">{status.detail}</span>
      ) : null}
    </span>
  );
}

// ── Sparkline ────────────────────────────────────────────────────────

function Sparkline({
  metric,
}: {
  metric: OverviewMetric;
}): React.JSX.Element | null {
  if (metric.history.length < 2) return null;
  const min = Math.min(...metric.history);
  const max = Math.max(...metric.history);
  const midpoint = (max + min) / 2;
  const stableRange = Math.max(Math.abs(midpoint) * 0.05, 1);
  const range = max - min;
  const points = metric.history
    .map((value, index) => {
      const x = (index / (metric.history.length - 1)) * 100;
      const y =
        range < stableRange
          ? 20 - ((value - midpoint) / stableRange) * 16
          : 36 - ((value - min) / range) * 32;
      return `${x.toFixed(1)},${y.toFixed(1)}`;
    })
    .join(" ");
  return (
    <svg
      aria-hidden="true"
      className="h-12 w-full opacity-70"
      preserveAspectRatio="none"
      viewBox="0 0 100 40"
    >
      <polyline
        fill="none"
        points={points}
        stroke="currentColor"
        strokeWidth="2.5"
        vectorEffect="non-scaling-stroke"
      />
    </svg>
  );
}

// ── MetricCard ───────────────────────────────────────────────────────

export function MetricCard({
  metric,
}: {
  metric: OverviewMetric;
}): React.JSX.Element {
  return (
    <section
      className={`${metricToneClasses[metric.key]} dashboard-stat-card-compact min-h-24 overflow-hidden border-l-2`}
    >
      <div className="text-base-content/70 text-[0.6875rem] font-semibold uppercase tracking-wide">
        {metric.label}
      </div>
      <div className="mt-1 grid grid-cols-[minmax(0,max-content)_minmax(2.5rem,1fr)] items-end gap-2">
        <div className="text-base-content min-w-0 text-xl font-semibold tabular-nums">
          {metric.value}
        </div>
        <div className="min-w-0">
          <Sparkline metric={metric} />
        </div>
      </div>
      <div
        className="text-base-content/70 mt-1 truncate text-xs"
        title={metric.note}
      >
        {metric.note}
      </div>
    </section>
  );
}

// ── Output filter helpers ────────────────────────────────────────────

export type OutputFilter = "all" | "attention" | "running" | "stopped";

export const outputFilters: readonly {
  readonly id: OutputFilter;
  readonly label: string;
}[] = [
  { id: "all", label: "All" },
  { id: "attention", label: "Attention" },
  { id: "running", label: "Running" },
  { id: "stopped", label: "Stopped" },
];

export function outputMatchesFilter(
  output: PipelineOutputCardModel,
  filter: OutputFilter,
): boolean {
  if (filter === "all") return true;
  if (filter === "attention")
    return output.status.tone === "warning" || output.status.tone === "error";
  if (filter === "running") return output.status.tone === "success";
  return output.status.label === "Stopped";
}

export function outputMatchesSearch(
  output: PipelineOutputCardModel,
  normalizedQuery: string,
): boolean {
  if (!normalizedQuery) return true;
  return [
    output.name,
    output.urlLabel,
    output.encodingLabel,
    output.status.label,
    output.status.detail ?? "",
  ]
    .join(" ")
    .toLowerCase()
    .includes(normalizedQuery);
}

export function outputExpansionLabel(
  model: PipelineOutputOverviewModel,
): string {
  if (model.expanded) return "Show fewer";
  const count = model.listCaption?.match(/\bof\s+(\d+)\s+outputs\b/)?.[1];
  return count ? `Show all ${count}` : "Show all";
}

export function outputStatusDetail(status: OverviewStatus): string {
  const detail = status.detail?.trim() ?? "";
  if (
    !detail ||
    detail === status.label ||
    detail === "Delivering media" ||
    detail === "Stopped by operator"
  ) {
    return "";
  }
  return detail;
}

// ── Time formatting ──────────────────────────────────────────────────

export function formatActivityTime(value: string | undefined): string {
  if (!value) return "Recent";
  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime())
    ? "Recent"
    : parsed.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

// ── Action-label formatting ──────────────────────────────────────────

export function normalizePendingActionLabel(label: string): string {
  return label.replace(/\.\.\.$/, "").trim();
}

export function formatPipelineHeaderFileIngestActionLabel(
  label: string,
): string {
  return normalizePendingActionLabel(label).replace(/\s+File$/, " file ingest");
}

export function formatPipelineHeaderRecordingActionLabel(
  label: string,
): string {
  const normalized = normalizePendingActionLabel(label);

  if (normalized === "Record") {
    return "Start recording";
  }

  return normalized.replace(/\s+Rec$/, " recording").replace(
    /^(Starting|Stopping)$/,
    "$1 recording",
  );
}
