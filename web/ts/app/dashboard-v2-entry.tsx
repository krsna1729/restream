import { useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import type { Root } from "react-dom/client";

import type {
  DashboardV2PipelineDetailsPlaceholder,
  DashboardV2OverviewActions,
  DashboardV2PipelineHeaderActions,
  DashboardV2PipelineInputStatusActions,
  DashboardV2PipelineOutputOverviewActions,
  DashboardV2PipelineSelectorActions,
} from "./dashboard-v2-loader.js";
import type {
  OverviewMetric,
  OverviewStatus,
  OverviewTone,
  OverviewViewModel,
} from "../features/overview-view-model.js";
import type {
  PipelineOperateHeaderModel,
  PipelineOperateInputStatusModel,
  PipelineOperateSelectorModel,
  PipelineOutputCardModel,
  PipelineOutputOverviewModel,
} from "../features/pipeline-operate-view-model.js";

const toneClasses: Readonly<Record<OverviewTone, string>> = {
  success: "border-success/30 bg-success/10 text-success",
  warning: "border-warning/35 bg-warning/10 text-warning",
  error: "border-error/35 bg-error/10 text-error",
  info: "border-info/30 bg-info/10 text-info",
  neutral: "border-base-content/10 bg-base-100/80 text-base-content/75",
};

const toneTextClasses: Readonly<Record<OverviewTone, string>> = {
  success: "text-success",
  warning: "text-warning",
  error: "text-error",
  info: "text-info",
  neutral: "text-base-content/75",
};

const INPUT_AUDIO_TRACK_PREVIEW_LIMIT = 6;

const metricToneClasses: Readonly<Record<OverviewMetric["key"], string>> = {
  inputs: "border-l-success text-success",
  outputs: "border-l-secondary text-secondary",
  inputKbps: "border-l-accent text-accent",
  outputKbps: "border-l-primary text-primary",
  engineCpu: "border-l-warning text-warning",
  engineMemory: "border-l-info text-info",
};

function Panel({
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

function StatusBadge({
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
        <span className="text-base-content/75 font-normal">
          {status.detail}
        </span>
      ) : null}
    </span>
  );
}

function outputStatusDetail(status: OverviewStatus): string {
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

type OutputFilter = "all" | "attention" | "running" | "stopped";

const outputFilters: readonly {
  readonly id: OutputFilter;
  readonly label: string;
}[] = [
  { id: "all", label: "All" },
  { id: "attention", label: "Attention" },
  { id: "running", label: "Running" },
  { id: "stopped", label: "Stopped" },
];

function outputMatchesFilter(
  output: PipelineOutputCardModel,
  filter: OutputFilter,
): boolean {
  if (filter === "all") return true;
  if (filter === "attention")
    return output.status.tone === "warning" || output.status.tone === "error";
  if (filter === "running") return output.status.tone === "success";
  return output.status.label === "Stopped";
}

function outputMatchesSearch(
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

function MetricCard({ metric }: { metric: OverviewMetric }): React.JSX.Element {
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

function formatActivityTime(value: string | undefined): string {
  if (!value) return "Recent";
  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime())
    ? "Recent"
    : parsed.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

function DashboardV2Overview({
  actions,
  model,
}: {
  actions: DashboardV2OverviewActions;
  model: OverviewViewModel;
}): React.JSX.Element {
  const hasAttention = model.attention.length > 0;
  return (
    <div className="space-y-4" id="dashboard-v2-overview">
      <header className="flex flex-wrap items-end justify-between gap-3 px-1">
        <div>
          <p className="text-accent text-xs font-semibold uppercase tracking-[0.18em]">
            Live operations
          </p>
          <h1 className="dashboard-title mt-1">Fleet overview</h1>
          <p className="dashboard-subtitle">
            See what needs action before scanning throughput and system load.
          </p>
        </div>
        <button
          className="btn btn-sm btn-primary"
          onClick={actions.addPipeline}
          type="button"
        >
          Add Pipeline
        </button>
      </header>

      <div className="grid items-start gap-4 lg:grid-cols-[minmax(0,1.2fr)_minmax(22rem,0.8fr)]">
        <Panel
          className={
            hasAttention
              ? "border-warning/35 bg-warning/5"
              : "border-success/25 bg-success/5"
          }
          labelledBy="dashboard-v2-attention-title"
        >
          <div className="dashboard-section-header items-start py-4">
            <div>
              <p
                className={`${hasAttention ? "text-warning" : "text-success"} text-xs font-semibold uppercase tracking-wider`}
              >
                Current priority
              </p>
              <h2
                className="mt-1 text-xl font-semibold"
                id="dashboard-v2-attention-title"
              >
                {hasAttention
                  ? `${model.attention.length} pipeline${model.attention.length === 1 ? "" : "s"} needs attention`
                  : model.counts.pipelines === 0
                    ? "Ready for the first pipeline"
                    : "Fleet is clear"}
              </h2>
              <p className="text-base-content/80 mt-1 text-sm">
                Issues are ordered by upstream cause and severity.
              </p>
            </div>
            <button
              className="btn btn-sm btn-outline"
              onClick={actions.openStatus}
              type="button"
            >
              Runtime detail
            </button>
          </div>
          <div
            className={`grid gap-3 p-4 ${model.attention.length > 1 ? "lg:grid-cols-2" : ""}`}
          >
            {hasAttention ? (
              model.attention.map((item) => (
                <article
                  className="dashboard-card p-3"
                  key={item.pipelineId}
                >
                  <div className="flex min-w-0 items-start justify-between gap-3">
                    <div className="min-w-0">
                      <h3 className="truncate font-semibold">
                        {item.pipelineName}
                      </h3>
                      <p className="text-base-content/70 mt-1 text-xs">
                        {item.detail}
                      </p>
                    </div>
                    <StatusBadge status={item.status} />
                  </div>
                  <div className="mt-3 flex flex-wrap gap-2">
                    <button
                      className="btn btn-xs btn-outline"
                      onClick={() => actions.openPipeline(item.pipelineId)}
                      type="button"
                    >
                      Operate
                    </button>
                    <button
                      className="btn btn-xs btn-outline"
                      onClick={() => actions.inspectPipeline(item.pipelineId)}
                      type="button"
                    >
                      Inspect
                    </button>
                  </div>
                </article>
              ))
            ) : (
              <div className="dashboard-empty">
                {model.counts.pipelines === 0
                  ? "Add a pipeline to begin monitoring inputs and destinations."
                  : "No active incident-level issues. Runtime detail stays available under Status and Pipeline Inspect."}
              </div>
            )}
          </div>
        </Panel>

        <Panel className="p-3" labelledBy="dashboard-v2-signals-title">
          <div className="mb-3 flex items-center justify-between gap-3 px-1">
            <div>
              <h2
                className="dashboard-section-title"
                id="dashboard-v2-signals-title"
              >
                Fleet signals
              </h2>
              <p className="dashboard-subtitle mt-0.5 text-xs">
                Current snapshot and recent trend
              </p>
            </div>
            <span className="badge badge-outline">
              {model.counts.pipelines} pipeline
              {model.counts.pipelines === 1 ? "" : "s"}
            </span>
          </div>
          <div className="grid grid-cols-2 gap-2">
            {model.metrics.map((metric) => (
              <MetricCard key={metric.key} metric={metric} />
            ))}
          </div>
        </Panel>
      </div>

      <Panel labelledBy="dashboard-v2-pipelines-title">
        <div className="dashboard-section-header">
          <div>
          <h2
            className="dashboard-section-title"
            id="dashboard-v2-pipelines-title"
          >
            All pipelines
          </h2>
          <p className="dashboard-subtitle">
            Compare intent, runtime state, and data flow.
          </p>
          </div>
        </div>
        <div className="overflow-x-auto">
          <table className="table table-sm">
            <thead className="text-base-content/70 bg-base-100/50 text-xs uppercase">
              <tr>
                <th>Pipeline</th>
                <th>State</th>
                <th>Input</th>
                <th>Outputs</th>
                <th>Input Rate</th>
                <th>Output Rate</th>
                <th>Recording</th>
              </tr>
            </thead>
            <tbody>
              {model.pipelines.length ? (
                model.pipelines.map((pipeline) => (
                  <tr
                    className="border-base-content/5 hover:bg-base-100/60 border-t"
                    key={pipeline.id}
                  >
                    <td className="min-w-56 py-3">
                      <button
                        className="group flex max-w-xs text-left"
                        onClick={() => actions.openPipeline(pipeline.id)}
                        type="button"
                      >
                        <span className="group-hover:text-accent truncate font-semibold">
                          {pipeline.name}
                        </span>
                      </button>
                    </td>
                    <td>
                      <StatusBadge status={pipeline.health} />
                    </td>
                    <td>
                      <StatusBadge status={pipeline.input} />
                    </td>
                    <td>
                      <StatusBadge status={pipeline.outputs} />
                    </td>
                    <td>
                      <StatusBadge status={pipeline.inputRate} />
                    </td>
                    <td>
                      <StatusBadge status={pipeline.outputRate} />
                    </td>
                    <td>
                      <StatusBadge status={pipeline.recording} />
                    </td>
                  </tr>
                ))
              ) : (
                <tr>
                  <td className="text-base-content/70 px-4 py-6" colSpan={7}>
                    No pipelines configured.
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      </Panel>

      <Panel labelledBy="dashboard-v2-activity-title">
        <div className="dashboard-section-header">
          <div>
            <h2
              className="dashboard-section-title"
              id="dashboard-v2-activity-title"
            >
              Restream Activity
            </h2>
            <p className="dashboard-subtitle">
              Recent restream-wide event bursts, grouped for operator-friendly
              review.
            </p>
          </div>
          <button
            className="btn btn-sm btn-outline"
            onClick={actions.openStatus}
            type="button"
          >
            Open Status
          </button>
        </div>
        <div className="space-y-2 p-4">
          {model.activityLoading ? (
            <p className="text-base-content/70 text-sm">
              Loading recent restream activity...
            </p>
          ) : model.activity.length ? (
            model.activity.map((item, index) => (
              <article
                className="dashboard-card p-3"
                key={`${item.startedAt || "activity"}-${index}`}
              >
                <div className="flex flex-wrap items-start justify-between gap-2">
                  <div>
                    <h3 className="font-semibold">{item.headline}</h3>
                    <p className="text-base-content/70 mt-1 text-sm">
                      {item.summary}
                    </p>
                  </div>
                  <span className={`${toneClasses[item.tone]} badge border`}>
                    {formatActivityTime(item.endedAt)}
                  </span>
                </div>
                <div className="mt-2 flex flex-wrap gap-1.5">
                  {item.details.map((detail) => (
                    <span className="badge badge-outline badge-sm" key={detail}>
                      {detail}
                    </span>
                  ))}
                  <span className="badge badge-ghost badge-sm">
                    {item.eventCount} event{item.eventCount === 1 ? "" : "s"}
                  </span>
                </div>
              </article>
            ))
          ) : (
            <p className="text-base-content/70 text-sm">
              No recent restream-wide activity yet.
            </p>
          )}
        </div>
      </Panel>
    </div>
  );
}

function DashboardV2PipelineSelector({
  actions,
  model,
}: {
  actions: DashboardV2PipelineSelectorActions;
  model: PipelineOperateSelectorModel;
}): React.JSX.Element {
  return (
    <section aria-labelledby="dashboard-v2-pipelines-selector-title">
      <div className="border-base-content/10 flex items-center justify-between gap-2 border-b px-4 py-3">
        <div>
          <h2
            className="text-base-content/70 text-sm font-semibold uppercase"
            id="dashboard-v2-pipelines-selector-title"
          >
            Pipelines
          </h2>
          <p className="text-base-content/50 mt-0.5 text-xs tabular-nums">
            {model.pipelines.length} configured
          </p>
        </div>
        <button
          className="btn btn-xs btn-accent btn-outline"
          onClick={actions.addPipeline}
          type="button"
        >
          Add
        </button>
      </div>
      {model.pipelines.length ? (
        <ul className="max-h-52 w-full space-y-1 overflow-x-hidden overflow-y-auto p-2 md:max-h-none">
          {model.pipelines.map((pipeline) => (
            <li className="min-w-0" key={pipeline.id}>
              <button
                aria-current={pipeline.selected ? "page" : undefined}
                className={`${pipeline.selected ? "bg-base-100 border-base-content/10" : "border-transparent"} hover:bg-base-100 flex w-full min-w-0 items-start gap-3 rounded-lg border px-3 py-2 text-left`}
                onClick={() => actions.selectPipeline(pipeline.id)}
                type="button"
              >
                <span
                  aria-hidden="true"
                  className={`${toneClasses[pipeline.statusTone]} mt-1 h-3 w-3 shrink-0 rounded-full border`}
                />
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-sm font-semibold">
                    {pipeline.name}
                  </span>
                  <span className="text-base-content/60 mt-1 block truncate text-xs">
                    {pipeline.statusLabel} · {pipeline.runningOutputs}/
                    {pipeline.totalOutputs} outputs
                  </span>
                  <span className="text-base-content/50 mt-1 flex flex-wrap gap-x-2 text-[0.6875rem] tabular-nums">
                    <span>{pipeline.inputRate} in</span>
                    <span>{pipeline.outputRate} out</span>
                  </span>
                </span>
              </button>
            </li>
          ))}
        </ul>
      ) : (
        <div className="text-base-content/60 px-4 py-5 text-sm">
          No pipelines configured.
        </div>
      )}
    </section>
  );
}

function DashboardV2PipelineHeader({
  actions,
  model,
}: {
  actions: DashboardV2PipelineHeaderActions;
  model: PipelineOperateHeaderModel;
}): React.JSX.Element {
  return (
    <section
      aria-labelledby="dashboard-v2-pipeline-title"
      className="space-y-2"
    >
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <h1
              className="whitespace-normal text-lg font-semibold leading-tight"
              id="dashboard-v2-pipeline-title"
            >
              {model.name}
            </h1>
            <StatusBadge status={model.health} />
          </div>
          <div className="text-base-content/60 mt-2 flex flex-wrap gap-x-3 gap-y-1 text-xs tabular-nums">
            <span>{model.sourceLabel}</span>
            <span>{model.inputRate} in</span>
            <span>{model.outputRate} out</span>
            <span>{model.outputsLabel}</span>
            <span>{model.recordingLabel}</span>
          </div>
        </div>
        <div className="flex shrink-0 flex-wrap gap-2">
          {model.fileIngestControl ? (
            <button
              className={`btn btn-xs ${
                model.fileIngestControl.danger ? "btn-error" : "btn-accent"
              } ${model.fileIngestControl.outlined ? "btn-outline" : ""}`}
              disabled={model.fileIngestControl.disabled}
              onClick={() => void actions.toggleFileIngest(model.id)}
              title={model.fileIngestControl.title}
              type="button"
            >
              {model.fileIngestControl.label}
            </button>
          ) : null}
          <button
            className={`btn btn-xs ${
              model.recordingControl.danger ? "btn-error" : "btn-accent"
            } ${model.recordingControl.outlined ? "btn-outline" : ""}`}
            disabled={model.recordingControl.disabled}
            onClick={() => void actions.toggleRecording(model.id)}
            title={model.recordingControl.title}
            type="button"
          >
            {model.recordingControl.label}
          </button>
          <button
            className="btn btn-xs btn-accent btn-outline"
            onClick={() => actions.inspectPipeline(model.id)}
            type="button"
          >
            Graph
          </button>
          <button
            className="btn btn-xs btn-accent btn-outline"
            disabled={!model.canDiagnose}
            onClick={() => actions.diagnosePipeline(model.id)}
            title={model.diagnoseDisabledReason || ""}
            type="button"
          >
            Diagnose
          </button>
          <button
            className="btn btn-xs btn-outline"
            disabled={!model.canEdit}
            onClick={() => actions.editPipeline(model.id)}
            title={model.editDisabledReason || ""}
            type="button"
          >
            Edit
          </button>
        </div>
      </div>
      {model.lifecycleMessages.length ? (
        <div aria-live="polite" className="space-y-2">
          {model.lifecycleMessages.map((message) => (
            <div
              className={`${toneClasses[message.tone]} rounded-lg border px-3 py-2 text-sm`}
              key={message.id}
              role="status"
            >
              <div className="font-semibold">{message.label}</div>
              <div className="mt-0.5 text-xs font-normal text-base-content/75">
                {message.detail}
              </div>
            </div>
          ))}
        </div>
      ) : null}
    </section>
  );
}

function DashboardV2PipelineDetailsPlaceholderCard({
  model,
}: {
  model: DashboardV2PipelineDetailsPlaceholder;
}): React.JSX.Element {
  return (
    <section
      aria-labelledby="dashboard-v2-pipeline-details-placeholder-title"
      className="dashboard-section border-info/25 bg-info/5"
    >
      <div className="flex items-start gap-3 py-2">
        <span
          aria-hidden="true"
          className="border-info/30 bg-info/10 mt-1 inline-flex h-3 w-3 shrink-0 rounded-full border"
        />
        <div>
          <h1
            className="text-base-content text-lg font-semibold leading-tight"
            id="dashboard-v2-pipeline-details-placeholder-title"
          >
            {model.title}
          </h1>
          <p className="text-base-content/65 mt-1 max-w-2xl text-sm">
            {model.message}
          </p>
        </div>
      </div>
    </section>
  );
}

function DashboardV2PipelineInputStatus({
  actions,
  model,
}: {
  actions: DashboardV2PipelineInputStatusActions;
  model: PipelineOperateInputStatusModel;
}): React.JSX.Element {
  const previewContainerRef = useRef<HTMLDivElement>(null);
  const [audioExpanded, setAudioExpanded] = useState(false);
  const audioTrackOverflow =
    model.audioTracks.length > INPUT_AUDIO_TRACK_PREVIEW_LIMIT;
  const visibleAudioTracks = audioExpanded
    ? model.audioTracks
    : model.audioTracks.slice(0, INPUT_AUDIO_TRACK_PREVIEW_LIMIT);

  useEffect(() => {
    const container = previewContainerRef.current;
    if (!container || !model.previewEnabled) return;
    actions.mountPreview(model.id, container);
    return () => actions.clearPreview(container);
  }, [
    actions,
    model.id,
    model.previewEnabled,
    model.previewKeyAssigned,
  ]);

  useEffect(() => {
    setAudioExpanded(false);
  }, [model.id]);

  return (
    <section
      aria-labelledby="dashboard-v2-input-status-title"
      className="mb-3 text-left"
    >
      <div className="flex flex-wrap items-start justify-between gap-2">
        <div>
          <h2
            className="text-base-content/70 text-xs font-semibold uppercase tracking-wide"
            id="dashboard-v2-input-status-title"
          >
            Input and preview
          </h2>
          <p className="text-base-content/55 mt-1 text-xs tabular-nums">
            {model.uptimeLabel}
          </p>
        </div>
        <StatusBadge status={model.status} />
      </div>
      <div className="border-base-content/10 divide-base-content/10 mt-3 grid border-y sm:grid-cols-3 sm:divide-x">
        <div className="border-base-content/10 px-1 py-2.5 sm:px-3">
          <div className="text-base-content/55 text-[0.7rem] font-semibold uppercase">
            Publisher
          </div>
          <div className="mt-1 flex flex-wrap items-center gap-2">
            <span className="text-sm font-medium">{model.publisherLabel}</span>
            {model.publisherHealth ? (
              <StatusBadge status={model.publisherHealth} />
            ) : null}
          </div>
          <p
            className="text-base-content/55 mt-1 truncate text-xs"
            title={model.publisherDetail}
          >
            {model.publisherDetail}
          </p>
        </div>
        <div className="border-base-content/10 border-t px-1 py-2.5 sm:border-t-0 sm:px-3">
          <div className="text-base-content/55 text-[0.7rem] font-semibold uppercase">
            Browser preview
          </div>
          <div className="mt-1">
            <StatusBadge status={model.preview} />
          </div>
          <p className="text-base-content/55 mt-1 text-xs tabular-nums">
            {model.previewDetail}
          </p>
        </div>
        <div className="border-base-content/10 border-t px-1 py-2.5 sm:border-t-0 sm:px-3">
          <div className="text-base-content/55 text-[0.7rem] font-semibold uppercase">
            Media
          </div>
          <p className="mt-1 text-sm font-medium">{model.videoLabel}</p>
          <p className="text-base-content/55 mt-1 text-xs">
            {model.audioLabel}
          </p>
        </div>
      </div>
      {model.previewEnabled ? (
        <div className="mt-3">
          <h3 className="text-base-content/60 mb-1 text-[0.7rem] font-semibold uppercase tracking-wide">
            Preview player
          </h3>
          <div
            data-role="dashboard-v2-input-preview"
            ref={previewContainerRef}
          />
        </div>
      ) : null}
      {model.unexpectedReadersLabel ? (
        <p className="text-error mt-2 text-xs font-medium">
          {model.unexpectedReadersLabel}
        </p>
      ) : null}
      {model.metricGroups.map((group) => (
        <div className="mt-3" key={group.key}>
          <h3 className="text-base-content/60 text-[0.7rem] font-semibold uppercase tracking-wide">
            {group.label}
          </h3>
          <dl className="border-base-content/10 mt-1 grid grid-cols-2 overflow-hidden rounded-md border sm:grid-cols-4">
            {group.metrics.map((metric, index) => (
              <div
                className={`${index % 2 === 1 ? "border-base-content/10 border-l" : ""} ${index > 1 ? "border-base-content/10 border-t sm:border-t-0" : ""} ${index > 0 ? "sm:border-base-content/10 sm:border-l" : ""} px-3 py-2`}
                key={metric.key}
              >
                <dt className="text-base-content/55 text-[0.7rem]">
                  {metric.label}
                </dt>
                <dd className="mt-1 text-sm font-medium tabular-nums">
                  {metric.value}
                </dd>
              </div>
            ))}
          </dl>
        </div>
      ))}
      <div className="mt-3">
        <h3 className="text-base-content/60 text-[0.7rem] font-semibold uppercase tracking-wide">
          Audio
        </h3>
        {model.audioTracks.length ? (
          <div className="border-base-content/10 divide-base-content/10 mt-1 divide-y border-y">
            {visibleAudioTracks.map((track) => (
              <div
                className="border-base-content/10 grid gap-2 px-1 py-2.5 sm:grid-cols-[minmax(0,1.2fr)_repeat(4,minmax(0,.7fr))] sm:px-3"
                key={track.key}
              >
                <div className="min-w-0">
                  <div className="text-base-content/55 text-[0.7rem]">
                    Track {track.index + 1}
                  </div>
                  {track.editing ? (
                    <div className="mt-1 flex flex-wrap gap-1">
                      <input
                        aria-label="Audio track name"
                        autoFocus
                        className="input input-bordered input-xs min-w-32 flex-1"
                        defaultValue={track.draft}
                        onChange={(event) =>
                          actions.updateAudioTrackDraft(
                            model.id,
                            track.key,
                            event.currentTarget.value,
                          )
                        }
                        onKeyDown={(event) => {
                          if (event.key === "Enter")
                            actions.saveAudioTrack(model.id, track.key);
                          if (event.key === "Escape")
                            actions.cancelAudioTrackEdit(model.id, track.key);
                        }}
                      />
                      <button
                        className="btn btn-xs btn-accent"
                        onClick={() =>
                          actions.saveAudioTrack(model.id, track.key)
                        }
                        type="button"
                      >
                        Save
                      </button>
                      <button
                        className="btn btn-xs btn-ghost"
                        onClick={() =>
                          actions.cancelAudioTrackEdit(model.id, track.key)
                        }
                        type="button"
                      >
                        Cancel
                      </button>
                    </div>
                  ) : (
                    <div className="mt-1 flex items-center gap-1">
                      <div className="min-w-0 flex-1">
                        <div className="truncate text-sm font-medium">
                          {track.label}
                        </div>
                        <div className="text-base-content/55 truncate text-xs">
                          {track.identity}
                        </div>
                      </div>
                      <button
                        aria-label={`Rename ${track.label}`}
                        className="btn btn-xs btn-ghost"
                        onClick={() =>
                          actions.editAudioTrack(model.id, track.key)
                        }
                        type="button"
                      >
                        Rename
                      </button>
                    </div>
                  )}
                </div>
                {[
                  ["Codec", track.codec],
                  ["Freq", track.sampleRate],
                  ["Channels", track.channels],
                  ["Profile", track.profile],
                ].map(([label, value]) => (
                  <div className="min-w-0" key={label}>
                    <div className="text-base-content/55 text-[0.7rem]">
                      {label}
                    </div>
                    <div className="mt-1 truncate text-sm">{value}</div>
                  </div>
                ))}
              </div>
            ))}
            {audioTrackOverflow ? (
              <div className="flex items-center justify-between gap-2 px-1 py-2.5 sm:px-3">
                <p className="text-base-content/55 text-xs">
                  Showing {visibleAudioTracks.length} of{" "}
                  {model.audioTracks.length} audio tracks
                </p>
                <button
                  className="btn btn-xs btn-outline"
                  onClick={() => setAudioExpanded((expanded) => !expanded)}
                  type="button"
                >
                  {audioExpanded
                    ? "Show fewer"
                    : `Show all ${model.audioTracks.length}`}
                </button>
              </div>
            ) : null}
          </div>
        ) : (
          <p className="text-base-content/55 mt-1 text-sm">No tracks</p>
        )}
      </div>
      {model.liveSource ? (
        <div className="border-base-content/10 mt-4 border-t pt-3">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div>
              <div className="text-base-content/55 text-[0.7rem] font-semibold uppercase">
                Stream key
              </div>
              <code className="mt-1 block text-sm">
                {model.liveSource.streamKeyLabel}
              </code>
            </div>
            <button
              className="btn btn-xs btn-accent btn-outline"
              onClick={() =>
                void actions.copyStreamKey(model.liveSource!.pipelineId)
              }
              type="button"
            >
              Copy Key
            </button>
          </div>
          <div className="mt-3 flex flex-wrap gap-2">
            {model.liveSource.protocols.map((protocol) => (
              <button
                aria-pressed={protocol.selected}
                className={`btn btn-xs ${protocol.selected ? "btn-accent" : "btn-outline"}`}
                key={protocol.id}
                onClick={() =>
                  actions.selectProtocol(
                    model.liveSource!.pipelineId,
                    protocol.id,
                  )
                }
                type="button"
              >
                {protocol.label}
              </button>
            ))}
          </div>
          {model.liveSource.protocols
            .filter(({ selected }) => selected)
            .map((protocol) => (
              <div className="mt-2 flex items-start gap-2" key={protocol.id}>
                <code className="bg-base-200 min-w-0 flex-1 rounded p-2 text-xs break-all">
                  {protocol.urlLabel}
                </code>
                <button
                  aria-label={`Copy ${protocol.label} ingest URL`}
                  className="btn btn-xs btn-outline"
                  onClick={() =>
                    void actions.copyIngestUrl(
                      model.liveSource!.pipelineId,
                      protocol.id,
                    )
                  }
                  type="button"
                >
                  Copy URL
                </button>
              </div>
            ))}
        </div>
      ) : null}
      {model.fileSource ? (
        <div className="border-base-content/10 mt-4 border-t pt-3">
          <div className="text-base-content/55 text-[0.7rem] font-semibold uppercase">
            Source file
          </div>
          <p
            className="mt-1 truncate text-sm font-medium"
            title={model.fileSource.filename}
          >
            {model.fileSource.filename}
          </p>
          {model.fileSource.warning ? (
            <div className="alert alert-warning mt-3 py-2 text-sm">
              {model.fileSource.warning}
            </div>
          ) : null}
          <dl className="mt-3 grid grid-cols-2 gap-2 sm:grid-cols-3">
            {model.fileSource.details.map((detail) => (
              <div
                className="bg-base-200/45 rounded-md px-3 py-2"
                key={detail.key}
              >
                <dt className="text-base-content/55 text-[0.7rem]">
                  {detail.label}
                </dt>
                <dd className="mt-1 text-sm font-medium tabular-nums">
                  {detail.value}
                </dd>
              </div>
            ))}
          </dl>
        </div>
      ) : null}
    </section>
  );
}

function DashboardV2PipelineOutputOverview({
  actions,
  model,
}: {
  actions: DashboardV2PipelineOutputOverviewActions;
  model: PipelineOutputOverviewModel;
}): React.JSX.Element {
  const [openActionsFor, setOpenActionsFor] = useState<string | null>(null);
  const [outputFilter, setOutputFilter] = useState<OutputFilter>("all");
  const [outputQuery, setOutputQuery] = useState("");
  const actionButtonRefs = useRef(new Map<string, HTMLButtonElement>());
  const normalizedOutputQuery = outputQuery.trim().toLowerCase();
  const filteredCards = model.cards.filter(
    (output) =>
      outputMatchesFilter(output, outputFilter) &&
      outputMatchesSearch(output, normalizedOutputQuery),
  );
  const filtersActive = outputFilter !== "all" || normalizedOutputQuery !== "";
  const showOutputTools = model.cards.length > 4 || filtersActive;
  const activeFilterLabel =
    outputFilters.find((filter) => filter.id === outputFilter)?.label ?? "All";
  const outputResultSummary = filtersActive
    ? `${filteredCards.length}/${model.cards.length} shown · ${activeFilterLabel}${
        normalizedOutputQuery ? ` · "${outputQuery.trim()}"` : ""
      }`
    : null;
  const outputEmptyDetail =
    outputFilter === "all"
      ? `No output destinations match "${outputQuery.trim()}".`
      : `No ${activeFilterLabel.toLowerCase()} output destinations match${
          normalizedOutputQuery ? ` "${outputQuery.trim()}"` : ""
        }.`;
  const closeActionsMenu = (outputId: string, restoreFocus = false): void => {
    setOpenActionsFor(null);
    if (restoreFocus) {
      window.requestAnimationFrame(() =>
        actionButtonRefs.current.get(outputId)?.focus(),
      );
    }
  };

  return (
    <section
      aria-labelledby="dashboard-v2-output-overview-title"
      className="mb-3"
    >
      <div className="flex flex-wrap items-start justify-between gap-2">
        <div>
          <h3
            className="text-base-content/70 text-xs font-semibold uppercase tracking-wide"
            id="dashboard-v2-output-overview-title"
          >
            Output overview
          </h3>
          <p className="text-base-content/55 mt-1 text-xs tabular-nums">
            {model.activeLabel} · {model.aggregateRate} aggregate
          </p>
        </div>
        <button
          className="btn btn-sm btn-accent btn-outline"
          onClick={() => actions.addOutput(model.pipelineId)}
          type="button"
        >
          Add Output
        </button>
      </div>
      {model.counts.length ? (
        <dl className="border-base-content/10 mt-3 flex flex-wrap gap-x-5 gap-y-2 border-y py-2.5">
          {model.counts.map((count) => (
            <div className="flex items-baseline gap-2" key={count.key}>
              <dt className="text-base-content/60 text-[0.65rem] font-semibold uppercase">
                {count.label}
              </dt>
              <dd
                className={`${toneTextClasses[count.tone]} text-lg font-semibold tabular-nums`}
              >
                {count.count}
              </dd>
            </div>
          ))}
        </dl>
      ) : (
        <p className="text-base-content/55 mt-3 text-sm">
          No outputs configured.
        </p>
      )}
      {model.attention.length ? (
        <div className="border-warning/30 mt-3 border-l-2 pl-3">
          <h4 className="text-warning text-xs font-semibold uppercase">
            Needs attention
          </h4>
          <div className="mt-2 space-y-2">
            {model.attention.map((output) => (
              <div
                className="border-base-content/10 border-t py-2 first:border-t-0"
                key={output.id}
              >
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <span className="min-w-0 truncate text-sm font-semibold">
                    {output.name}
                  </span>
                  <StatusBadge showDetail={false} status={output.status} />
                </div>
                <p className="text-base-content/55 mt-1 text-xs tabular-nums">
                  {output.encodingLabel} · {output.rateLabel}
                </p>
              </div>
            ))}
          </div>
        </div>
      ) : model.counts.length ? (
        <p className="text-success mt-3 text-xs font-medium">
          No outputs need attention.
        </p>
      ) : null}
      {model.cards.length ? (
        <div className="mt-3 space-y-2">
          <div className="space-y-2">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <h4 className="text-base-content/70 text-xs font-semibold uppercase">
                Output destinations
              </h4>
              {filtersActive ? (
                <span
                  aria-live="polite"
                  className="text-base-content/55 text-xs tabular-nums"
                  role="status"
                >
                  {outputResultSummary}
                </span>
              ) : null}
            </div>
            {showOutputTools ? (
              <>
                <label className="input input-bordered input-sm flex min-h-10 items-center gap-2">
                  <span className="text-base-content/55 text-xs font-semibold uppercase">
                    Find
                  </span>
                  <input
                    aria-label="Search output destinations"
                    className="min-w-0 grow"
                    onChange={(event) =>
                      setOutputQuery(event.currentTarget.value)
                    }
                    placeholder="name, URL, state"
                    type="search"
                    value={outputQuery}
                  />
                </label>
                <div
                  aria-label="Filter output destinations by state"
                  className="flex flex-wrap gap-1"
                  role="group"
                >
                  {outputFilters.map((filter) => (
                    <button
                      aria-pressed={outputFilter === filter.id}
                      className={`btn btn-xs ${
                        outputFilter === filter.id
                          ? "btn-accent"
                          : "btn-outline btn-ghost"
                      }`}
                      key={filter.id}
                      onClick={() => setOutputFilter(filter.id)}
                      type="button"
                    >
                      {filter.label}
                    </button>
                  ))}
                </div>
              </>
            ) : null}
          </div>
          {filteredCards.length ? (
            filteredCards.map((output) => {
              const detail = outputStatusDetail(output.status);
              return (
                <article
                  className="border-base-content/10 border-t py-2.5"
                  key={output.id}
                >
              <div className="flex flex-wrap items-start justify-between gap-2">
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-2">
                    <h5 className="min-w-0 truncate text-sm font-semibold">
                      {output.name}
                    </h5>
                    <StatusBadge showDetail={false} status={output.status} />
                  </div>
                  <p className="text-base-content/60 mt-1 text-xs tabular-nums">
                    {output.encodingLabel} · {output.rateLabel}
                    {output.uptimeLabel
                      ? ` · ${output.uptimeLabel} uptime`
                      : ""}
                  </p>
                  <code className="text-base-content/50 mt-1 block truncate text-xs">
                    {output.urlLabel}
                  </code>
                  {detail ? (
                    <p className="text-base-content/55 mt-1 text-xs">
                      {detail}
                    </p>
                  ) : null}
                  {output.controlError ? (
                    <p
                      className="text-error mt-1 text-xs font-medium"
                      role="status"
                    >
                      Output request failed · {output.controlError}
                    </p>
                  ) : null}
                </div>
                <button
                  aria-label={`${output.controlLabel.replace("...", "")} ${output.name}`}
                  className="btn btn-sm btn-accent btn-outline"
                  disabled={output.controlDisabled}
                  onClick={() =>
                    void actions.toggleOutput(model.pipelineId, output.id)
                  }
                  type="button"
                >
                  {output.controlLabel}
                </button>
              </div>
              <div className="relative mt-2 flex justify-end">
                <button
                  aria-haspopup="menu"
                  aria-expanded={openActionsFor === output.id}
                  aria-label={`More actions for ${output.name}`}
                  className="btn btn-xs btn-ghost"
                  onClick={() =>
                    setOpenActionsFor((current) =>
                      current === output.id ? null : output.id,
                    )
                  }
                  ref={(element) => {
                    if (element) {
                      actionButtonRefs.current.set(output.id, element);
                    } else {
                      actionButtonRefs.current.delete(output.id);
                    }
                  }}
                  type="button"
                >
                  More
                </button>
                {openActionsFor === output.id ? (
                  <div
                    aria-label={`More actions for ${output.name}`}
                    className="bg-base-100 border-base-content/10 absolute right-0 top-7 z-20 w-36 rounded-lg border p-1 shadow-xl"
                    onKeyDown={(event) => {
                      if (event.key === "Escape") {
                        event.stopPropagation();
                        closeActionsMenu(output.id, true);
                      }
                    }}
                    role="menu"
                  >
                    <button
                      aria-label={`History ${output.name}`}
                      className="btn btn-xs btn-ghost w-full justify-start"
                      onClick={() => {
                        closeActionsMenu(output.id);
                        actions.openOutputHistory(
                          model.pipelineId,
                          output.id,
                          output.name,
                        );
                      }}
                      role="menuitem"
                      type="button"
                    >
                      History
                    </button>
                    {output.monitorAvailable ? (
                      <button
                        aria-label={`Monitor ${output.name}`}
                        className="btn btn-xs btn-ghost w-full justify-start"
                        onClick={() => {
                          closeActionsMenu(output.id);
                          actions.monitorOutput(model.pipelineId, output.id);
                        }}
                        role="menuitem"
                        type="button"
                      >
                        Monitor
                      </button>
                    ) : null}
                    <button
                      aria-label={`Edit ${output.name}`}
                      className="btn btn-xs btn-ghost w-full justify-start"
                      onClick={() => {
                        closeActionsMenu(output.id);
                        actions.editOutput(model.pipelineId, output.id);
                      }}
                      role="menuitem"
                      type="button"
                    >
                      Edit
                    </button>
                    <button
                      aria-label={`Delete ${output.name}`}
                      className="btn btn-xs btn-ghost text-error w-full justify-start"
                      disabled={output.deleteDisabled}
                      onClick={() => {
                        closeActionsMenu(output.id);
                        void actions.deleteOutput(model.pipelineId, output.id);
                      }}
                      role="menuitem"
                      type="button"
                    >
                      Delete
                    </button>
                  </div>
                ) : null}
              </div>
            </article>
            );
          })
          ) : (
            <div className="border-base-content/10 rounded-lg border border-dashed px-3 py-4">
              <p className="text-sm font-semibold">No outputs match.</p>
              <p
                aria-live="polite"
                className="text-base-content/60 mt-1 text-xs"
                role="status"
              >
                {outputEmptyDetail} Clear filters to return to all
                destinations.
              </p>
              <button
                className="btn btn-xs btn-ghost mt-3"
                onClick={() => {
                  setOutputFilter("all");
                  setOutputQuery("");
                }}
                type="button"
              >
                Clear filters
              </button>
            </div>
          )}
          {model.canExpand ? (
            <div className="flex flex-wrap items-center justify-between gap-2 pt-1">
              <p className="text-base-content/55 text-xs">
                {model.listCaption}
              </p>
              <button
                className="btn btn-xs btn-ghost"
                onClick={() => actions.toggleOutputList(model.pipelineId)}
                type="button"
              >
                {model.expanded ? "Show less" : "Show all"}
              </button>
            </div>
          ) : null}
        </div>
      ) : null}
    </section>
  );
}

const dashboardV2Container = document.getElementById("dashboard-v2-root");
if (!dashboardV2Container)
  throw new Error("Dashboard v2 experiment root is missing");
const container: HTMLElement = dashboardV2Container;
container.dataset.uiV2Seam = "UI v2 seam active";
let root: Root | null = null;
const pipelineSelectorContainer = document.getElementById(
  "dashboard-v2-pipeline-selector-root",
);
if (!pipelineSelectorContainer) {
  throw new Error("Dashboard v2 pipeline selector root is missing");
}
const selectorContainer: HTMLElement = pipelineSelectorContainer;
let selectorRoot: Root | null = null;
const pipelineHeaderContainer = document.getElementById(
  "dashboard-v2-pipeline-header-root",
);
if (!pipelineHeaderContainer) {
  throw new Error("Dashboard v2 pipeline header root is missing");
}
const headerContainer: HTMLElement = pipelineHeaderContainer;
let headerRoot: Root | null = null;
const pipelineInputStatusContainer = document.getElementById(
  "dashboard-v2-pipeline-input-status-root",
);
if (!pipelineInputStatusContainer) {
  throw new Error("Dashboard v2 pipeline input status root is missing");
}
const inputStatusContainer: HTMLElement = pipelineInputStatusContainer;
let inputStatusRoot: Root | null = null;
const pipelineOutputOverviewContainer = document.getElementById(
  "dashboard-v2-pipeline-output-overview-root",
);
if (!pipelineOutputOverviewContainer) {
  throw new Error("Dashboard v2 pipeline output overview root is missing");
}
const outputOverviewContainer: HTMLElement = pipelineOutputOverviewContainer;
let outputOverviewRoot: Root | null = null;

export function renderDashboardV2Overview(
  model: OverviewViewModel,
  actions: DashboardV2OverviewActions,
): void {
  container.hidden = false;
  root ??= createRoot(container);
  root.render(<DashboardV2Overview actions={actions} model={model} />);
}

export function renderDashboardV2PipelineSelector(
  model: PipelineOperateSelectorModel,
  actions: DashboardV2PipelineSelectorActions,
): void {
  selectorContainer.hidden = false;
  selectorRoot ??= createRoot(selectorContainer);
  selectorRoot.render(
    <DashboardV2PipelineSelector actions={actions} model={model} />,
  );
}

export function renderDashboardV2PipelineHeader(
  model: PipelineOperateHeaderModel | null,
  actions: DashboardV2PipelineHeaderActions,
  placeholder: DashboardV2PipelineDetailsPlaceholder | null = null,
): void {
  headerRoot ??= createRoot(headerContainer);
  headerContainer.hidden = model === null && placeholder === null;
  headerRoot.render(
    model ? (
      <DashboardV2PipelineHeader actions={actions} model={model} />
    ) : placeholder ? (
      <DashboardV2PipelineDetailsPlaceholderCard model={placeholder} />
    ) : null,
  );
}

export function renderDashboardV2PipelineInputStatus(
  model: PipelineOperateInputStatusModel | null,
  actions: DashboardV2PipelineInputStatusActions,
): void {
  inputStatusRoot ??= createRoot(inputStatusContainer);
  inputStatusContainer.hidden = model === null;
  inputStatusRoot.render(
    model ? (
      <DashboardV2PipelineInputStatus actions={actions} model={model} />
    ) : null,
  );
}

export function renderDashboardV2PipelineOutputOverview(
  model: PipelineOutputOverviewModel | null,
  actions: DashboardV2PipelineOutputOverviewActions,
): void {
  outputOverviewRoot ??= createRoot(outputOverviewContainer);
  outputOverviewContainer.hidden = model === null;
  outputOverviewRoot.render(
    model ? (
      <DashboardV2PipelineOutputOverview actions={actions} model={model} />
    ) : null,
  );
}
