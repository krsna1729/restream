import type { PipelineView } from "../../types.js";
import type { ControlRoomCheckpointModel } from "./view-model.js";

interface ControlRoomCheckpointInputs {
  allMonitoringOutputs: Array<{
    monitoringUrl: string | null;
    status: string;
  }>;
  filteredMonitoringOutputCount: number;
  lazyWebPreviewCount: number;
  searchQuery: string;
  selectedPipeline: PipelineView | null;
}

function pluralize(
  count: number,
  singular: string,
  plural = `${singular}s`,
): string {
  return `${count} ${count === 1 ? singular : plural}`;
}

export function controlRoomScopeSummaryText(
  selectedPipeline: PipelineView | null,
  monitoredOutputCount: number,
): string {
  if (!selectedPipeline) return "Monitoring wall · no pipeline selected";
  const totalOutputs = selectedPipeline.outs.length;
  const missingMonitoring = totalOutputs - monitoredOutputCount;
  return `Monitoring ${selectedPipeline.name} · ${pluralize(totalOutputs, "output")} · ${pluralize(monitoredOutputCount, "monitor")} · ${pluralize(missingMonitoring, "missing URL")}`;
}

export function buildControlRoomCheckpointModel({
  allMonitoringOutputs,
  filteredMonitoringOutputCount,
  lazyWebPreviewCount,
  searchQuery,
  selectedPipeline,
}: ControlRoomCheckpointInputs): ControlRoomCheckpointModel {
  if (!selectedPipeline) {
    return {
      pipelineId: null,
      title: "Monitoring wall",
      summary: "No pipeline is selected.",
      statusLabel: "No selection",
      statusTone: "neutral",
      monitoredLabel: "0 monitored",
      missingLabel: "No pipeline",
      searchLabel: "No active search",
      previewLabel: "No previews",
      focusLabel: "Select a pipeline to see local preview and output monitors.",
      nextStep: "Choose a pipeline from the monitor selector.",
      canOpenPipeline: false,
      metrics: [],
    };
  }

  const totalOutputs = selectedPipeline.outs.length;
  const missingMonitoring = totalOutputs - allMonitoringOutputs.length;
  const query = searchQuery.trim();
  const downMonitors = allMonitoringOutputs.filter((output) =>
    ["failed", "off", "stopped"].includes(
      (output.status || "").trim().toLowerCase(),
    ),
  ).length;
  const statusTone =
    allMonitoringOutputs.length === 0 || downMonitors > 0
      ? "warning"
      : missingMonitoring > 0
        ? "neutral"
        : "success";
  const statusLabel =
    allMonitoringOutputs.length === 0
      ? "Needs URLs"
      : downMonitors > 0
        ? `${pluralize(downMonitors, "monitor")} down`
        : missingMonitoring > 0
          ? "Partially covered"
          : "Covered";
  const nextStep = query
    ? filteredMonitoringOutputCount
      ? "Clear search when you are done with the narrowed monitor set."
      : "Clear search or add a matching monitoring URL."
    : missingMonitoring > 0
      ? "Add missing monitoring URLs before treating the wall as complete."
      : lazyWebPreviewCount > 0
        ? "Load web previews only when the operator needs them."
        : "Use the monitor wall for live output confirmation.";

  return {
    pipelineId: selectedPipeline.id,
    title: selectedPipeline.name,
    summary: controlRoomScopeSummaryText(
      selectedPipeline,
      allMonitoringOutputs.length,
    ),
    statusLabel,
    statusTone,
    monitoredLabel: `${allMonitoringOutputs.length}/${totalOutputs} monitored`,
    missingLabel: pluralize(missingMonitoring, "missing URL"),
    searchLabel: query
      ? `${filteredMonitoringOutputCount}/${allMonitoringOutputs.length} match "${query}"`
      : "No active search",
    previewLabel: lazyWebPreviewCount
      ? `${pluralize(lazyWebPreviewCount, "lazy web preview")}`
      : "No lazy web previews",
    focusLabel: query
      ? `${pluralize(filteredMonitoringOutputCount, "visible monitor")} after search · ${pluralize(missingMonitoring, "missing URL")}`
      : `${pluralize(allMonitoringOutputs.length, "configured monitor")} · ${pluralize(missingMonitoring, "missing URL")} · ${pluralize(lazyWebPreviewCount, "lazy web preview")}`,
    nextStep,
    canOpenPipeline: true,
    metrics: [
      { label: "Outputs", value: String(totalOutputs) },
      { label: "Configured", value: String(allMonitoringOutputs.length) },
      { label: "Visible", value: String(filteredMonitoringOutputCount) },
      { label: "Lazy", value: String(lazyWebPreviewCount) },
    ],
  };
}
