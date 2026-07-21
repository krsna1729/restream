import { escapeRedactedHtml } from "../../core/utils.js";
import type { PipelineHistoryState } from "../state.js";
import { formatHistoryTime, getCorrelationId, renderCorrelationBadge } from "./utils.js";
import { classifyPipelineHistoryEvent, getPipelineTimelineLogs } from "./classify.js";
import { buildPipelineIncidents } from "./incidents.js";
import { getOrderedOutputLogs } from "./utils.js";

export function renderPipelineHistory(
  state: PipelineHistoryState,
  { scrollToTop = false }: { scrollToTop?: boolean } = {},
): void {
  const list = document.getElementById("pipeline-history-list");
  const empty = document.getElementById("pipeline-history-empty");

  if (!list || !empty) return;

  list.replaceChildren();

  if (!Array.isArray(state.logs) || state.logs.length === 0) {
    empty.classList.remove("hidden");
    return;
  }

  empty.classList.add("hidden");

  const logs = getOrderedOutputLogs(getPipelineTimelineLogs(state.logs), "asc");
  const incidents = buildPipelineIncidents(logs).reverse();
  list.innerHTML = incidents
    .map((incident, incidentIndex) => {
      const timeRange =
        incident.startedAt &&
        incident.endedAt &&
        incident.startedAt !== incident.endedAt
          ? `${formatHistoryTime(incident.startedAt)} -> ${formatHistoryTime(incident.endedAt)}`
          : formatHistoryTime(incident.endedAt || incident.startedAt);
      const detailsHtml =
        incident.detailBadges.length > 0
          ? `<div class="mt-2 flex flex-wrap gap-1">${incident.detailBadges
              .map(
                (detail) =>
                  `<span class="border-base-content/10 bg-base-200/70 rounded-md border px-2 py-1 text-[11px]">${escapeRedactedHtml(detail, true)}</span>`,
              )
              .join("")}</div>`
          : "";
      const correlationHtml =
        incident.correlationIds.length > 0
          ? `<div class="mt-2 flex flex-wrap gap-1">${incident.correlationIds
              .slice(0, 2)
              .map((correlationId) => renderCorrelationBadge(correlationId))
              .join("")}${
              incident.correlationIds.length > 2
                ? `<span class="badge badge-sm badge-ghost">+${incident.correlationIds.length - 2} more</span>`
                : ""
            }</div>`
          : "";

      return `<div class="border-base-content/10 bg-base-100 rounded-xl border p-3">
                <div class="flex items-start justify-between gap-3">
                    <div class="min-w-0">
                        <div class="flex flex-wrap items-center gap-2">
                            <span class="badge badge-sm ${incident.badgeClass}">${incident.headline}</span>
                            <span class="badge badge-sm badge-ghost">${incident.logs.length} event${incident.logs.length === 1 ? "" : "s"}</span>
                        </div>
                        <p class="mt-2 text-sm opacity-80">${incident.summary}</p>
                        ${correlationHtml}
                        ${detailsHtml}
                    </div>
                    <div class="shrink-0 text-right text-xs opacity-70">${timeRange}</div>
                </div>
                <div class="mt-3 space-y-2">${incident.logs
                  .map((log, logIndex) => {
                    const event = classifyPipelineHistoryEvent(log);
                    const correlationId = getCorrelationId(log);
                    return `<div class="border-base-content/10 bg-base-200/45 rounded-lg border p-2">
                                <div class="flex items-center justify-between gap-2">
                                    <div class="flex flex-wrap items-center gap-2">
                                        <span class="badge badge-xs ${event.badgeClass}">${event.label}</span>
                                        ${correlationId ? renderCorrelationBadge(correlationId, "badge-xs") : ""}
                                    </div>
                                    <span class="text-[11px] opacity-70">${formatHistoryTime(log.ts)}</span>
                                </div>
                                <pre class="mt-1 text-xs whitespace-pre-wrap break-words js-incident-msg" data-incident-index="${incidentIndex}" data-log-index="${logIndex}"></pre>
                            </div>`;
                  })
                  .join("")}</div>
            </div>`;
    })
    .join("");
  list.querySelectorAll<HTMLPreElement>(".js-incident-msg").forEach((pre) => {
    const incidentIndex = Number(pre.dataset.incidentIndex);
    const logIndex = Number(pre.dataset.logIndex);
    pre.textContent = String(
      incidents[incidentIndex]?.logs[logIndex]?.message || "",
    );
  });

  if (scrollToTop) list.scrollTop = 0;
}
