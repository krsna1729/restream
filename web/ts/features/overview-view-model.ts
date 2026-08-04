import {
  isOutputFlapping,
  isOutputIntentStopped,
  isOutputRunning,
  isOutputRetrying,
  isOutputUnexpectedlyDown,
} from "../core/output-status.js";
import type { PipelineView, SystemMetrics } from "../types.js";

export type OverviewTone = "success" | "warning" | "error" | "info" | "neutral";

export type OverviewMetricKey =
  | "inputs"
  | "outputs"
  | "inputKbps"
  | "outputKbps"
  | "engineCpu"
  | "engineMemory";

export interface OverviewStatus {
  readonly label: string;
  readonly tone: OverviewTone;
  readonly detail?: string;
}

export interface OverviewFleetCounts {
  readonly pipelines: number;
  readonly liveInputs: number;
  readonly warningInputs: number;
  readonly outputs: number;
  readonly runningOutputs: number;
  readonly retryingOutputs: number;
  readonly flappingOutputs: number;
  readonly stoppedOutputs: number;
  readonly downOutputs: number;
  readonly recording: number;
  readonly inputKbps: number;
  readonly outputKbps: number;
}

export interface OverviewAttentionItem {
  readonly pipelineId: string;
  readonly pipelineName: string;
  readonly status: OverviewStatus;
  readonly detail: string;
}

export interface OverviewPipelineRow {
  readonly id: string;
  readonly name: string;
  readonly health: OverviewStatus;
  readonly input: OverviewStatus;
  readonly outputs: OverviewStatus;
  readonly inputRate: OverviewStatus;
  readonly outputRate: OverviewStatus;
  readonly recording: OverviewStatus;
}

export interface OverviewMetric {
  readonly key: OverviewMetricKey;
  readonly label: string;
  readonly value: string;
  readonly note: string;
  readonly history: readonly number[];
}

export interface OverviewActivityItem {
  readonly headline: string;
  readonly summary: string;
  readonly details: readonly string[];
  readonly evidence: string;
  readonly eventCount: number;
  readonly startedAt?: string;
  readonly endedAt?: string;
  readonly tone: OverviewTone;
}

export interface OverviewPresentationInput {
  readonly activityBursts?: readonly OverviewActivityBurstInput[];
  readonly activityLoading?: boolean;
  readonly metricHistory?: Partial<
    Readonly<Record<OverviewMetricKey, readonly number[]>>
  >;
}

export interface OverviewActivityBurstInput {
  readonly badgeClass: string;
  readonly detailBadges: readonly string[];
  readonly endedAt?: string;
  readonly headline: string;
  readonly logs: readonly unknown[];
  readonly summary: string;
  readonly startedAt?: string;
}

export interface OverviewViewModel {
  readonly counts: OverviewFleetCounts;
  readonly attentionPipelines: number;
  readonly attention: readonly OverviewAttentionItem[];
  readonly pipelines: readonly OverviewPipelineRow[];
  readonly metrics: readonly OverviewMetric[];
  readonly activity: readonly OverviewActivityItem[];
  readonly activityLoading: boolean;
}

export function formatOverviewBitrate(kbps: number | null | undefined): string {
  if (!Number.isFinite(kbps as number) || (kbps as number) < 0) return "--";
  const value = kbps as number;
  return value >= 1000
    ? `${(value / 1000).toFixed(1)} Mb/s`
    : `${value.toFixed(0)} Kb/s`;
}

// Also used by pipeline-inspector, which duplicated these until this move —
// kept here (rather than core/utils.ts) because this module is imported by
// Node-only view-model tests that don't have a `window` global, and
// core/utils.ts has a module-load-time `window.copyData = ...` side effect.
export function formatAgeMs(ms: number | null | undefined): string {
  if (!Number.isFinite(ms as number) || (ms as number) < 0) return "--";
  const seconds = Math.round((ms as number) / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  const remainder = seconds % 60;
  return remainder ? `${minutes}m ${remainder}s` : `${minutes}m`;
}

export function formatByteSize(bytes: number | null | undefined): string {
  if (!Number.isFinite(bytes as number) || (bytes as number) <= 0) return "--";
  const value = bytes as number;
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KiB`;
  if (value < 1024 * 1024 * 1024)
    return `${(value / (1024 * 1024)).toFixed(1)} MiB`;
  return `${(value / (1024 * 1024 * 1024)).toFixed(1)} GiB`;
}

function inputIsStalled(pipe: PipelineView): boolean {
  return (
    pipe.input.status === "on" &&
    Number.isFinite(pipe.input.lastProgressAgeMs as number) &&
    (pipe.input.lastProgressAgeMs as number) >= 10_000
  );
}

function formatPercent(value: number | null | undefined): string {
  if (!Number.isFinite(value as number) || (value as number) < 0) return "--";
  return `${(value as number).toFixed((value as number) >= 10 ? 0 : 1)}%`;
}

function hasMetricValue(value: number | null | undefined): boolean {
  return Number.isFinite(value as number) && (value as number) >= 0;
}

export function pipelineOverviewHealth(pipe: PipelineView): OverviewStatus {
  if (pipe.input.status === "error")
    return { label: "Input error", tone: "error", detail: "publisher fault" };
  if (pipe.input.status === "warning") {
    return {
      label: pipe.input.flapping ? "Input flapping" : "Input warning",
      tone: "warning",
      detail: pipe.input.flapping
        ? `${Math.max(pipe.input.recentDisconnectCount || 0, 2)} recent drops`
        : "check ingest",
    };
  }
  if (pipe.input.status !== "on") {
    return pipe.outs.some(isOutputUnexpectedlyDown)
      ? { label: "Input down", tone: "error", detail: "outputs blocked" }
      : { label: "Idle", tone: "neutral", detail: "waiting for input" };
  }
  if (!pipe.input.probeReady)
    return {
      label: "Input probing",
      tone: "warning",
      detail: "waiting for stream metadata",
    };
  if (inputIsStalled(pipe))
    return {
      label: "Input stalled",
      tone: "warning",
      detail: `no progress for ${formatAgeMs(pipe.input.lastProgressAgeMs)}`,
    };
  if (pipe.outs.some((out) => out.status === "stalled"))
    return { label: "Output stalled", tone: "warning", detail: "no progress" };
  if (pipe.outs.some(isOutputUnexpectedlyDown))
    return { label: "Output down", tone: "error", detail: "input live" };
  if (pipe.outs.some(isOutputRetrying))
    return { label: "Output retrying", tone: "warning", detail: "recovering" };
  if (pipe.outs.some(isOutputFlapping))
    return {
      label: "Output flapping",
      tone: "warning",
      detail: "recent sink drops",
    };
  if (pipe.outs.some((out) => out.status === "warning"))
    return { label: "Output warning", tone: "warning", detail: "input live" };
  if (pipe.input.flapping)
    return {
      label: "Input flapping",
      tone: "warning",
      detail: `${Math.max(pipe.input.recentDisconnectCount || 0, 2)} recent drops`,
    };
  return { label: "Live", tone: "success", detail: "healthy" };
}

export function pipelineNeedsOverviewAttention(pipe: PipelineView): boolean {
  return (
    pipe.input.status === "error" ||
    pipe.input.status === "warning" ||
    (pipe.input.status === "on" &&
      (!pipe.input.probeReady || pipe.input.flapping || inputIsStalled(pipe))) ||
    pipe.outs.some(
      (output) =>
        isOutputUnexpectedlyDown(output) ||
        isOutputRetrying(output) ||
        isOutputFlapping(output) ||
        output.status === "warning",
    )
  );
}

function inputStatus(pipe: PipelineView): OverviewStatus {
  const protocol = pipe.input.publisher?.protocol?.toUpperCase();
  const rate = formatOverviewBitrate(pipe.stats.inputBitrateKbps);
  if (pipe.input.status === "on" && !pipe.input.probeReady) {
    const pendingMs = pipe.input.probePendingMs;
    return {
      label: "Input probing",
      tone: "warning",
      detail:
        Number.isFinite(pendingMs as number) && Number(pendingMs) > 0
          ? `${protocol || "publisher"} / ${(Number(pendingMs) / 1000).toFixed(1)}s`
          : protocol || "publisher",
    };
  }
  if (inputIsStalled(pipe)) {
    return {
      label: "Input stalled",
      tone: "warning",
      detail: [
        protocol || "publisher",
        `${formatByteSize(pipe.input.bytesReceived)} received`,
        `stale ${formatAgeMs(pipe.input.lastProgressAgeMs)}`,
      ].join(" / "),
    };
  }
  if (pipe.input.status === "on") {
    return pipe.input.flapping
      ? {
          label: "Input flapping",
          tone: "warning",
          detail: `${Math.max(pipe.input.recentDisconnectCount || 0, 2)} recent drops${protocol ? ` / ${protocol}` : ""}`,
        }
      : {
          label: "Live input",
          tone: "success",
          detail: [protocol, rate !== "--" ? rate : null]
            .filter(Boolean)
            .join(" / "),
        };
  }
  if (pipe.input.status === "warning")
    return {
      label: pipe.input.flapping ? "Input flapping" : "Input warning",
      tone: "warning",
      detail: pipe.input.flapping
        ? `${Math.max(pipe.input.recentDisconnectCount || 0, 2)} recent drops`
        : protocol || "publisher attached",
    };
  if (pipe.input.status === "error")
    return {
      label: "Input error",
      tone: "error",
      detail: protocol || "publisher fault",
    };
  return {
    label: "No input",
    tone: "neutral",
    detail: pipe.inputSource ? "file/source idle" : "waiting",
  };
}

function outputsStatus(pipe: PipelineView): OverviewStatus {
  const total = pipe.outs.length;
  const running = pipe.outs.filter(isOutputRunning).length;
  const retrying = pipe.outs.filter(isOutputRetrying).length;
  const flapping = pipe.outs.filter(isOutputFlapping).length;
  const stopped = pipe.outs.filter(isOutputIntentStopped).length;
  const down = pipe.outs.filter(isOutputUnexpectedlyDown).length;
  if (!total)
    return { label: "No outputs", tone: "neutral", detail: "not configured" };
  if (pipe.input.status !== "on" && down > 0)
    return {
      label: `${running}/${total} running`,
      tone: "neutral",
      detail: "blocked by input",
    };
  if (down > 0)
    return {
      label: `${down} down`,
      tone: "error",
      detail: `${running}/${total} running`,
    };
  if (retrying > 0)
    return {
      label: `${retrying} retrying`,
      tone: "warning",
      detail: `${running}/${total} running`,
    };
  if (flapping > 0)
    return {
      label: `${flapping} flapping`,
      tone: "warning",
      detail: `${running}/${total} running`,
    };
  if (stopped === total)
    return { label: "Stopped", tone: "neutral", detail: `${total} configured` };
  if (running === total)
    return { label: `${running}/${total} running`, tone: "success" };
  return {
    label: `${running}/${total} running`,
    tone: "warning",
    detail: `${stopped} stopped`,
  };
}

function attentionItem(pipe: PipelineView): OverviewAttentionItem | null {
  if (!pipelineNeedsOverviewAttention(pipe)) return null;
  const status = pipelineOverviewHealth(pipe);
  const down = pipe.outs.filter(isOutputUnexpectedlyDown).length;
  const retrying = pipe.outs.filter(isOutputRetrying).length;
  const flapping = pipe.outs.filter(isOutputFlapping).length;
  const warnings = pipe.outs.filter(
    (output) => output.status === "warning",
  ).length;
  const details = [
    down ? `${down} down` : "",
    retrying ? `${retrying} retrying` : "",
    flapping ? `${flapping} flapping` : "",
    warnings ? `${warnings} warning` : "",
    pipe.input.flapping
      ? `${Math.max(pipe.input.recentDisconnectCount || 0, 2)} input drops`
      : "",
    inputIsStalled(pipe)
      ? `input stale ${formatAgeMs(pipe.input.lastProgressAgeMs)}`
      : "",
  ].filter(Boolean);
  return {
    pipelineId: pipe.id,
    pipelineName: pipe.name,
    status,
    detail: details.join(" / ") || status.detail || "check pipeline",
  };
}

function activityTone(burst: OverviewActivityBurstInput): OverviewTone {
  if (burst.badgeClass.includes("error")) return "error";
  if (burst.badgeClass.includes("warning")) return "warning";
  if (burst.badgeClass.includes("success")) return "success";
  return "neutral";
}

function activityEvidence(burst: OverviewActivityBurstInput): string {
  const detail = burst.detailBadges.slice(0, 2).join(" / ");
  const eventLabel = `${burst.logs.length} log${burst.logs.length === 1 ? "" : "s"} reviewed`;
  return detail ? `${eventLabel} / ${detail}` : eventLabel;
}

export function buildOverviewViewModel(
  pipelines: readonly PipelineView[],
  systemMetrics: SystemMetrics = {},
  presentation: OverviewPresentationInput = {},
): OverviewViewModel {
  const outputs = pipelines.flatMap((pipe) => pipe.outs);
  const counts: OverviewFleetCounts = {
    pipelines: pipelines.length,
    liveInputs: pipelines.filter(
      (pipe) =>
        pipe.input.status === "on" &&
        pipe.input.probeReady &&
        !pipe.input.flapping &&
        !inputIsStalled(pipe),
    ).length,
    warningInputs: pipelines.filter(
      (pipe) =>
        pipe.input.status === "warning" ||
        (pipe.input.status === "on" &&
          (!pipe.input.probeReady ||
            pipe.input.flapping ||
            inputIsStalled(pipe))),
    ).length,
    outputs: outputs.length,
    runningOutputs: outputs.filter(isOutputRunning).length,
    retryingOutputs: outputs.filter(isOutputRetrying).length,
    flappingOutputs: outputs.filter(isOutputFlapping).length,
    stoppedOutputs: outputs.filter(isOutputIntentStopped).length,
    downOutputs: outputs.filter(isOutputUnexpectedlyDown).length,
    recording: pipelines.filter((pipe) => pipe.recording.active).length,
    inputKbps: pipelines.reduce(
      (sum, pipe) => sum + (pipe.stats.inputBitrateKbps || 0),
      0,
    ),
    outputKbps: pipelines.reduce(
      (sum, pipe) => sum + (pipe.stats.outputBitrateKbps || 0),
      0,
    ),
  };
  const engine = systemMetrics.engine || {};
  const ffmpegCount = Number(engine.externalFfmpegCount || 0);
  const ffmpegMemory = Number(engine.externalFfmpegMemoryBytes || 0);
  const restreamMemory = Number(
    engine.restreamMemoryBytes ?? engine.memoryBytes ?? 0,
  );
  const engineMemory = Number(
    engine.totalMemoryBytes || restreamMemory + ffmpegMemory,
  );
  const history = (key: OverviewMetricKey): readonly number[] =>
    presentation.metricHistory?.[key] || [];
  const detail = (parts: string[], fallback = "warming..."): string =>
    parts.filter((part) => part.trim()).join(" / ") || fallback;
  const attention = pipelines
    .map(attentionItem)
    .filter((item): item is OverviewAttentionItem => item !== null)
    .sort((left, right) => {
      const severity =
        Number(right.status.tone === "error") -
        Number(left.status.tone === "error");
      return severity || left.pipelineName.localeCompare(right.pipelineName);
    })
    .slice(0, 4);

  return {
    counts,
    attentionPipelines: pipelines.filter(pipelineNeedsOverviewAttention).length,
    attention,
    pipelines: [...pipelines]
      .sort((left, right) => left.name.localeCompare(right.name))
      .map((pipe) => ({
        id: pipe.id,
        name: pipe.name,
        health: pipelineOverviewHealth(pipe),
        input: inputStatus(pipe),
        outputs: outputsStatus(pipe),
        inputRate: {
          label: formatOverviewBitrate(pipe.stats.inputBitrateKbps),
          tone: Number.isFinite(pipe.stats.inputBitrateKbps as number)
            ? "info"
            : "neutral",
        },
        outputRate: {
          label: formatOverviewBitrate(pipe.stats.outputBitrateKbps),
          tone: Number.isFinite(pipe.stats.outputBitrateKbps as number)
            ? "info"
            : "neutral",
        },
        recording: pipe.recording.active
          ? { label: "Recording", tone: "error", detail: "active" }
          : pipe.recording.enabled
            ? { label: "Armed", tone: "warning", detail: "ready" }
            : { label: "Off", tone: "neutral" },
      })),
    metrics: [
      {
        key: "inputs",
        label: "Inputs live",
        value: `${counts.liveInputs}/${counts.pipelines}`,
        note: counts.warningInputs
          ? `${counts.warningInputs} warning`
          : "All quiet",
        history: history("inputs"),
      },
      {
        key: "outputs",
        label: "Outputs running",
        value: `${counts.runningOutputs}/${counts.outputs}`,
        note: counts.retryingOutputs
          ? `${counts.retryingOutputs} retrying`
          : counts.downOutputs
            ? `${counts.downOutputs} down`
            : "Desired state met",
        history: history("outputs"),
      },
      {
        key: "inputKbps",
        label: "Inbound",
        value: formatOverviewBitrate(counts.inputKbps),
        note: "Active publishers",
        history: history("inputKbps"),
      },
      {
        key: "outputKbps",
        label: "Outbound",
        value: formatOverviewBitrate(counts.outputKbps),
        note: `${counts.recording} recording${counts.recording === 1 ? "" : "s"}`,
        history: history("outputKbps"),
      },
      {
        key: "engineCpu",
        label: "Engine CPU",
        value: formatPercent(engine.cpuPercent),
        note: detail([
          hasMetricValue(engine.restreamCpuPercent)
            ? `Restream ${formatPercent(engine.restreamCpuPercent)}`
            : "",
          ffmpegCount > 0 && hasMetricValue(engine.externalFfmpegCpuPercent)
            ? `FFmpeg ${formatPercent(engine.externalFfmpegCpuPercent)} (${ffmpegCount})`
            : "",
        ]),
        history: history("engineCpu"),
      },
      {
        key: "engineMemory",
        label: "Engine memory",
        value: formatByteSize(engineMemory),
        note: detail(
          [
            hasMetricValue(restreamMemory) && restreamMemory > 0
              ? `Restream ${formatByteSize(restreamMemory)}`
              : "",
            ffmpegCount > 0 && hasMetricValue(ffmpegMemory) && ffmpegMemory > 0
              ? `FFmpeg ${formatByteSize(ffmpegMemory)}`
              : "",
          ],
          "No engine memory sample",
        ),
        history: history("engineMemory"),
      },
    ],
    activity: (presentation.activityBursts || []).map((burst) => ({
      headline: burst.headline,
      summary: burst.summary,
      details: burst.detailBadges,
      evidence: activityEvidence(burst),
      eventCount: burst.logs.length,
      startedAt: burst.startedAt,
      endedAt: burst.endedAt,
      tone: activityTone(burst),
    })),
    activityLoading: presentation.activityLoading === true,
  };
}
