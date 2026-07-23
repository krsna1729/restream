import { escapeHtml } from "../../core/utils.js";
import type { OperatorAlert, ResourceMapSnapshot } from "../../core/api.js";
import type { PipelineView } from "../../types.js";
import {
  inspectFaultCandidates,
  inspectProbeBlockers,
  inspectSuggestedNextStep,
} from "./view-helpers.js";
import {
  resourceDetailPanelHtml,
  resourceSummaryGroupsHtml,
} from "./resource-view.js";

let inspectProbeDetailsExpanded = false;
let inspectResourceDetailsExpanded = false;

function pluralize(n: number, singular: string): string {
  return `${n} ${n === 1 ? singular : `${singular}s`}`;
}

function titleCaseValue(value: string | null | undefined): string {
  if (!value) return "--";
  const normalized = String(value).trim();
  if (!normalized) return "--";
  return normalized.charAt(0).toUpperCase() + normalized.slice(1).toLowerCase();
}

function alertSeverityBadgeClass(severity: string | undefined): string {
  if (severity === "critical" || severity === "error") return "badge-error";
  if (severity === "warning") return "badge-warning";
  return "badge-info";
}

export function operatorAlertText(text: string, pipe?: PipelineView): string {
  let display = text || "";
  if (pipe && pipe.outs) {
    for (const output of pipe.outs) {
      if (!output.id || !output.name) continue;
      display = display.replaceAll(output.id, output.name);
    }
    display = display.replace(/\bOutput '([^']+)'/g, "$1");
    if (pipe.id) display = display.replaceAll(`${pipe.id}:`, "");
  }
  return display;
}

export function alertSummaryHtml(
  alerts: OperatorAlert[] | null,
  pipe: PipelineView,
): string {
  if (alerts === null) {
    return `<section class="border-base-content/10 bg-base-100/40 mt-3 rounded-lg border px-2.5 py-2">
      <div class="text-base-content/55 text-[0.68rem] font-semibold uppercase tracking-wide">Active Alerts</div>
      <div class="text-base-content/60 mt-1 text-sm">Loading alert details.</div>
    </section>`;
  }
  if (alerts.length === 0) {
    return `<section class="border-base-content/10 bg-base-100/40 mt-3 rounded-lg border px-2.5 py-2">
      <div class="text-base-content/55 text-[0.68rem] font-semibold uppercase tracking-wide">Active Alerts</div>
      <div class="text-base-content/60 mt-1 text-sm">No active operator alerts.</div>
    </section>`;
  }
  return `<section class="border-warning/30 bg-warning/5 mt-3 rounded-lg border px-2.5 py-2">
    <div class="mb-1.5 flex items-center justify-between gap-2">
      <div class="text-warning text-[0.68rem] font-semibold uppercase tracking-wide">Active Alerts</div>
      <span class="text-warning text-xs font-medium tabular-nums">${escapeHtml(String(alerts.length))}</span>
    </div>
    <div class="max-h-64 space-y-2 overflow-y-auto pr-1" data-scroll-preserve="pipeline-alerts">
      ${alerts
        .map(
          (alert: any) => `<div class="border-warning/20 border-t pt-2 first:border-t-0 first:pt-0">
            <div class="flex min-w-0 items-center justify-between gap-2">
              <div class="min-w-0 truncate text-sm font-medium">${escapeHtml(operatorAlertText(alert.title || alert.text || alert.message || "", pipe))}</div>
              <span class="badge ${alertSeverityBadgeClass(alert.severity || alert.level)} badge-sm shrink-0">${escapeHtml(titleCaseValue(alert.severity || alert.level || "warning"))}</span>
            </div>
            ${alert.cause ? `<div class="text-base-content/65 mt-0.5 line-clamp-2 text-xs">${escapeHtml(operatorAlertText(alert.cause, pipe))}</div>` : ""}
            ${alert.recommendedAction ? `<div class="text-base-content/50 mt-1 text-xs">${escapeHtml(operatorAlertText(alert.recommendedAction, pipe))}</div>` : ""}
          </div>`,
        )
        .join("")}
    </div>
  </section>`;
}

export function renderDiagnostics(pipe: PipelineView | null): void {
  const container = document.getElementById("inspect-diagnostics-summary");
  if (!container) return;
  const focusSummary = document.getElementById("inspect-focus-summary");
  if (!pipe) {
    if (focusSummary) {
      focusSummary.textContent =
        "Inspection focus · select a pipeline to inspect diagnostics.";
    }
    container.innerHTML =
      '<div class="text-base-content/60 text-sm">Select a pipeline to inspect diagnostics.</div>';
    return;
  }

  const blockers = inspectProbeBlockers(pipe);
  const faultCandidates = inspectFaultCandidates(pipe);
  const suggestedNextStep = inspectSuggestedNextStep(pipe);
  const blockerText = blockers.length
    ? blockers.map(escapeHtml).join("<br>")
    : "Ready for active diagnostics.";
  const faultText = faultCandidates.length
    ? faultCandidates.map((out: any) => escapeHtml(out.name)).join("<br>")
    : "No unexpected output failures.";
  if (focusSummary) {
    focusSummary.textContent = `Inspection focus · ${blockers.length ? `${pluralize(blockers.length, "blocker")} before active probes` : "ready for active probes"} · ${pluralize(faultCandidates.length, "fault candidate")} · ${suggestedNextStep}`;
  }

  const detailLabel = `${
    inspectProbeDetailsExpanded ? "Hide" : "Show"
  } probe details for ${pipe.name}`;
  container.innerHTML = `<section class="border-base-content/10 bg-base-100/35 rounded-lg border p-3">
        <div class="flex flex-wrap items-start justify-between gap-3">
          <div>
            <div class="dashboard-kicker">Probe plan</div>
            <div class="mt-1 text-sm">${escapeHtml(suggestedNextStep)}</div>
            <div class="text-base-content/55 mt-1 text-xs">${escapeHtml(pluralize(blockers.length, "blocker"))} · ${escapeHtml(pluralize(faultCandidates.length, "fault candidate"))}</div>
          </div>
          <button id="inspect-probe-details-toggle" type="button" class="btn btn-xs btn-outline" aria-label="${escapeHtml(detailLabel)}" aria-expanded="${inspectProbeDetailsExpanded ? "true" : "false"}">${inspectProbeDetailsExpanded ? "Hide probe details" : "Show probe details"}</button>
        </div>
        ${
          inspectProbeDetailsExpanded
            ? `<div class="mt-3 grid gap-3 md:grid-cols-2">
                <div class="dashboard-stat-card-compact">
                  <div class="dashboard-kicker">Probe Readiness</div>
                  <div class="mt-2 text-sm">${blockerText}</div>
                </div>
                <div class="dashboard-stat-card-compact">
                  <div class="dashboard-kicker">Fault Candidates</div>
                  <div class="mt-2 text-sm">${faultText}</div>
                </div>
              </div>`
            : ""
        }
      </section>`;
  document
    .getElementById("inspect-probe-details-toggle")
    ?.addEventListener("click", () => {
      inspectProbeDetailsExpanded = !inspectProbeDetailsExpanded;
      renderDiagnostics(pipe);
    });
}

export function renderInspectorResourceDetails(
  pipe: PipelineView | null,
  resourceMap: ResourceMapSnapshot | null,
): void {
  const container = document.getElementById("inspect-resource-details");
  if (!container) return;
  if (!pipe) {
    container.innerHTML =
      '<div class="text-base-content/60 text-sm">Select a pipeline to inspect its FFmpeg workers and resource attribution.</div>';
    return;
  }
  if (!resourceMap) {
    container.innerHTML =
      '<div class="text-base-content/60 text-sm">Resource details are loading.</div>';
    return;
  }
  const detailHtml = resourceDetailPanelHtml(resourceMap, pipe);
  const resourceDetailsLabel = `${inspectResourceDetailsExpanded ? "Hide" : "Show"} resource details for ${pipe.name}`;
  const resourceDetailPanel = `<section class="border-base-content/10 bg-base-100/35 rounded-lg border p-3">
        <div class="flex flex-wrap items-center justify-between gap-2">
          <div>
            <div class="text-sm font-semibold">Resource detail tables</div>
          </div>
          <button id="inspect-resource-details-toggle" type="button" class="btn btn-xs btn-outline" aria-label="${escapeHtml(resourceDetailsLabel)}" aria-expanded="${inspectResourceDetailsExpanded ? "true" : "false"}">${inspectResourceDetailsExpanded ? "Hide resource details" : "Show resource details"}</button>
        </div>
        ${inspectResourceDetailsExpanded ? `<div class="mt-3">${detailHtml}</div>` : ""}
      </section>`;
  container.innerHTML = `<div class="space-y-3">
    ${resourceSummaryGroupsHtml(resourceMap)}
    ${resourceDetailPanel}
  </div>`;
  document
    .getElementById("inspect-resource-details-toggle")
    ?.addEventListener("click", () => {
      inspectResourceDetailsExpanded = !inspectResourceDetailsExpanded;
      renderInspectorResourceDetails(pipe, resourceMap);
    });
}
