import {
  getAggregateAlerts,
  getLifecycleEvents,
  getOverview,
} from "../core/api.js";
import { escapeHtml } from "../core/utils.js";
import type {
  AlertsSnapshot,
  LifecycleEvent,
  LifecycleEventsSnapshot,
  OperatorAlert,
  OverviewSnapshot,
} from "../core/api.js";
import type { IncidentsCheckpointModel } from "./incidents-view-model.js";

export interface IncidentPipelineOption {
  id: string;
  name: string;
}

export interface IncidentSnapshot {
  overview: OverviewSnapshot | null;
  alerts: AlertsSnapshot | null;
  events: LifecycleEventsSnapshot | null;
  loaded: boolean;
  unavailable: boolean;
}

interface IncidentsViewOptions {
  active: boolean;
  pipelines: IncidentPipelineOption[];
  navigateToPipeline: (pipelineId: string) => void;
}

const INCIDENT_REFRESH_MS = 5_000;
const INCIDENT_EVENT_LIMIT = 60;
const INCIDENT_ALERT_GROUP_VISIBLE_LIMIT = 8;
const INCIDENT_EVENT_VISIBLE_LIMIT = 12;
let selectedPipelineId = "";
let lastFetchedAt = 0;
let requestSequence = 0;
const inFlightByScope = new Map<
  string,
  { sequence: number; promise: Promise<void> }
>();
let snapshot: IncidentSnapshot = {
  overview: null,
  alerts: null,
  events: null,
  loaded: false,
  unavailable: false,
};
let viewOptions: IncidentsViewOptions | null = null;
let incidentSearchQuery = "";
let incidentAlertGroupsExpanded = false;
let incidentEventsExpanded = false;
let incidentsCheckpointCallback:
  | ((model: IncidentsCheckpointModel | null) => void)
  | null = null;

export function configureIncidentsCheckpointPresentation(options: {
  readonly onPresentation?: (model: IncidentsCheckpointModel | null) => void;
}): void {
  incidentsCheckpointCallback = options.onPresentation ?? null;
  if (!incidentsCheckpointCallback || !viewOptions?.active) {
    incidentsCheckpointCallback?.(null);
    return;
  }
  incidentsCheckpointCallback(
    buildIncidentsCheckpointModel(
      snapshot,
      viewOptions.pipelines,
      selectedPipelineId,
      incidentSearchQuery,
    ),
  );
}

function formatTime(value: string | undefined): string {
  const millis = Date.parse(value || "");
  return Number.isFinite(millis)
    ? new Date(millis).toLocaleString()
    : "Unknown";
}

function alertMatchesPipeline(
  alert: OperatorAlert,
  pipelineId: string,
): boolean {
  return !pipelineId || alert.pipelineId === pipelineId;
}

function severityTone(severity: OperatorAlert["severity"]): string {
  return severity === "critical" ? "badge-error" : "badge-warning";
}

interface AlertGroup {
  id: string;
  severity: OperatorAlert["severity"];
  pipelineId?: string;
  stageId?: string;
  stageIds: string[];
  title: string;
  cause: string;
  recommendedAction: string;
  alerts: OperatorAlert[];
  outputIds: string[];
  firstSeen?: string;
  lastSeen?: string;
}

function alertGroupKey(alert: OperatorAlert): string {
  if (alert.stageId) {
    return [
      alert.severity,
      alert.pipelineId || "",
      "stage",
      alert.title,
      alert.recommendedAction || "",
    ].join("|");
  }
  return [
    alert.severity,
    alert.pipelineId || "",
    alert.stageId || "",
    alert.cause || alert.title,
    alert.recommendedAction || "",
  ].join("|");
}

function groupAlerts(alerts: OperatorAlert[]): AlertGroup[] {
  const groups = new Map<string, AlertGroup>();
  for (const alert of alerts) {
    const key = alertGroupKey(alert);
    const existing =
      groups.get(key) ||
      ({
        id: key,
        severity: alert.severity,
        pipelineId: alert.pipelineId,
        stageId: alert.stageId,
        stageIds: [],
        title: alert.stageId
          ? `Upstream stage blocked outputs`
          : alert.title,
        cause: alert.cause,
        recommendedAction: alert.recommendedAction,
        alerts: [],
        outputIds: [],
        firstSeen: alert.firstSeen,
        lastSeen: alert.lastSeen || alert.generatedAt,
      } satisfies AlertGroup);
    existing.alerts.push(alert);
    if (alert.stageId && !existing.stageIds.includes(alert.stageId)) {
      existing.stageIds.push(alert.stageId);
    }
    if (alert.outputId && !existing.outputIds.includes(alert.outputId)) {
      existing.outputIds.push(alert.outputId);
    }
    const firstSeenMs = Date.parse(existing.firstSeen || "");
    const alertFirstSeenMs = Date.parse(alert.firstSeen || alert.generatedAt);
    if (
      Number.isFinite(alertFirstSeenMs) &&
      (!Number.isFinite(firstSeenMs) || alertFirstSeenMs < firstSeenMs)
    ) {
      existing.firstSeen = alert.firstSeen || alert.generatedAt;
    }
    const lastSeenMs = Date.parse(existing.lastSeen || "");
    const alertLastSeenMs = Date.parse(alert.lastSeen || alert.generatedAt);
    if (
      Number.isFinite(alertLastSeenMs) &&
      (!Number.isFinite(lastSeenMs) || alertLastSeenMs > lastSeenMs)
    ) {
      existing.lastSeen = alert.lastSeen || alert.generatedAt;
    }
    groups.set(key, existing);
  }
  return [...groups.values()].sort((left, right) => {
    const severity =
      Number(right.severity === "critical") -
      Number(left.severity === "critical");
    return (
      severity ||
      right.alerts.length - left.alerts.length ||
      left.title.localeCompare(right.title)
    );
  });
}

function eventSummary(event: LifecycleEvent): string {
  if (event.error) return `${event.kind}: ${event.error}`;
  if (event.outputId) return `${event.kind}: ${event.outputId}`;
  if (event.encoding) return `${event.kind}: ${event.encoding}`;
  if (event.protocol) return `${event.kind}: ${event.protocol.toUpperCase()}`;
  return event.kind;
}

function normalizeSearch(value: string): string {
  return value.trim().toLowerCase();
}

function alertSearchText(alert: OperatorAlert): string {
  return [
    alert.id,
    alert.severity,
    alert.scope,
    alert.pipelineId,
    alert.stageId,
    alert.outputId,
    alert.title,
    alert.cause,
    alert.recommendedAction,
    ...(alert.evidence || []),
  ]
    .filter(Boolean)
    .join(" ")
    .toLowerCase();
}

function eventSearchText(event: LifecycleEvent): string {
  return [
    eventSummary(event),
    event.kind,
    event.pipelineId,
    event.outputId,
    event.protocol,
    event.encoding,
    event.error,
  ]
    .filter(Boolean)
    .join(" ")
    .toLowerCase();
}

function renderAlert(alert: OperatorAlert): string {
  const evidence = (alert.evidence || [])
    .map((item) => `<li>${escapeHtml(item)}</li>`)
    .join("");
  const target = [alert.pipelineId, alert.stageId, alert.outputId]
    .filter(Boolean)
    .join(" / ");
  return `<article class="border-base-content/10 bg-base-100 rounded-lg border p-4" data-alert-id="${escapeHtml(alert.id)}">
    <div class="flex flex-wrap items-start justify-between gap-2">
      <div><h3 class="font-semibold">${escapeHtml(alert.title)}</h3><p class="text-base-content/60 mt-1 text-xs">${escapeHtml(target || alert.scope)}</p></div>
      <span class="badge ${severityTone(alert.severity)}">${escapeHtml(alert.severity)}</span>
    </div>
    <p class="mt-3 text-sm">${escapeHtml(alert.cause)}</p>
    ${evidence ? `<details class="mt-3 text-sm"><summary class="cursor-pointer font-medium">Evidence</summary><ul class="mt-2 list-disc space-y-1 pl-5">${evidence}</ul></details>` : ""}
    <div class="bg-base-200 mt-3 rounded-md p-3 text-sm"><span class="font-medium">Recommended action:</span> ${escapeHtml(alert.recommendedAction)}</div>
    <div class="mt-3 flex flex-wrap items-center justify-between gap-2 text-xs text-base-content/60">
      <span>Last seen ${escapeHtml(formatTime(alert.lastSeen || alert.generatedAt))}</span>
      ${alert.pipelineId ? `<button type="button" class="btn btn-xs btn-outline" data-open-incident-pipeline="${escapeHtml(alert.pipelineId)}">Open pipeline</button>` : ""}
    </div>
  </article>`;
}

function renderAlertGroup(group: AlertGroup): string {
  if (group.alerts.length === 1) return renderAlert(group.alerts[0]);
  const stageCount = group.stageIds.length;
  const title =
    stageCount > 1 && group.title.includes("stage")
      ? group.title.replace("stage", "stages")
      : group.title;
  const cause =
    stageCount > 1
      ? `${stageCount} upstream stages are not delivering packets to dependent outputs.`
      : group.cause;
  const evidence = group.alerts
    .flatMap((alert) => alert.evidence || [])
    .filter((item, index, all) => all.indexOf(item) === index)
    .slice(0, 5)
    .map((item) => `<li>${escapeHtml(item)}</li>`)
    .join("");
  const sampleOutputs = group.outputIds.slice(0, 6);
  const remainingOutputs = Math.max(0, group.outputIds.length - sampleOutputs.length);
  const impactGridClass = stageCount
    ? "sm:grid-cols-[8rem_8rem_1fr]"
    : "sm:grid-cols-[8rem_1fr]";
  return `<article class="border-base-content/10 bg-base-100 rounded-lg border p-4" data-alert-group-id="${escapeHtml(group.id)}">
    <div class="flex flex-wrap items-start justify-between gap-3">
      <div class="min-w-0">
        <h3 class="font-semibold">${escapeHtml(title)}</h3>
        <p class="text-base-content/60 mt-1 text-xs">${escapeHtml(group.pipelineId || "fleet")}${stageCount ? ` / ${stageCount} stages` : group.stageId ? ` / ${escapeHtml(group.stageId)}` : ""}</p>
      </div>
      <div class="flex shrink-0 items-center gap-2">
        <span class="badge badge-outline">${group.alerts.length} alerts</span>
        <span class="badge ${severityTone(group.severity)}">${escapeHtml(group.severity)}</span>
      </div>
    </div>
    <p class="mt-3 text-sm">${escapeHtml(cause)}</p>
    <div class="mt-3 grid gap-2 ${impactGridClass}">
      <div class="bg-base-200 rounded-md p-3">
        <div class="text-base-content/60 text-xs font-semibold uppercase">Blast radius</div>
        <div class="mt-1 text-lg font-semibold tabular-nums">${group.outputIds.length || group.alerts.length}</div>
        <div class="text-base-content/60 text-xs">${group.outputIds.length ? "outputs affected" : "conditions"}</div>
      </div>
      ${
        stageCount
          ? `<div class="bg-base-200 rounded-md p-3">
        <div class="text-base-content/60 text-xs font-semibold uppercase">Stages</div>
        <div class="mt-1 text-lg font-semibold tabular-nums">${stageCount}</div>
        <div class="text-base-content/60 text-xs">upstream</div>
      </div>`
          : ""
      }
      <div class="bg-base-200 rounded-md p-3 text-sm">
        <span class="font-medium">Recommended action:</span> ${escapeHtml(group.recommendedAction)}
      </div>
    </div>
    ${sampleOutputs.length ? `<details class="mt-3 text-sm"><summary class="cursor-pointer font-medium">Affected outputs</summary><div class="mt-2 flex flex-wrap gap-1">${sampleOutputs.map((id) => `<code class="bg-base-200 rounded px-1.5 py-1 text-xs">${escapeHtml(id)}</code>`).join("")}${remainingOutputs ? `<span class="text-base-content/60 px-1.5 py-1 text-xs">+${remainingOutputs} more</span>` : ""}</div></details>` : ""}
    ${evidence ? `<details class="mt-3 text-sm"><summary class="cursor-pointer font-medium">Evidence</summary><ul class="mt-2 list-disc space-y-1 pl-5">${evidence}</ul></details>` : ""}
    <div class="mt-3 flex flex-wrap items-center justify-between gap-2 text-xs text-base-content/60">
      <span>Last seen ${escapeHtml(formatTime(group.lastSeen))}</span>
      ${group.pipelineId ? `<button type="button" class="btn btn-xs btn-outline" data-open-incident-pipeline="${escapeHtml(group.pipelineId)}">Open pipeline</button>` : ""}
    </div>
  </article>`;
}

function renderEvent(event: LifecycleEvent): string {
  return `<li class="border-base-content/10 border-b py-3 last:border-0">
    <div class="flex items-start justify-between gap-3"><span class="text-sm font-medium">${escapeHtml(eventSummary(event))}</span><time datetime="${escapeHtml(event.timestamp)}" class="text-base-content/50 whitespace-nowrap text-xs">${escapeHtml(formatTime(event.timestamp))}</time></div>
    <div class="text-base-content/60 mt-1 text-xs">${escapeHtml(event.pipelineId)}</div>
  </li>`;
}

function pluralize(
  count: number,
  singular: string,
  plural = `${singular}s`,
): string {
  return `${count} ${count === 1 ? singular : plural}`;
}

function incidentScopeLabel(
  pipelines: IncidentPipelineOption[],
  pipelineId: string,
): string {
  if (!pipelineId) return "fleet";
  return (
    pipelines.find((pipeline) => pipeline.id === pipelineId)?.name ||
    pipelineId
  );
}

function incidentScopeCardLabel(scopeLabel: string): string {
  return scopeLabel === "fleet" ? "Fleet" : scopeLabel;
}

function incidentStatusTone(
  data: IncidentSnapshot,
  critical: number,
  warning: number,
): IncidentsCheckpointModel["statusTone"] {
  if (!data.loaded) return "neutral";
  if (critical > 0) return "error";
  if (warning > 0 || data.unavailable) return "warning";
  return "success";
}

function buildIncidentsCheckpointModel(
  data: IncidentSnapshot,
  pipelines: IncidentPipelineOption[],
  pipelineId: string,
  searchQuery = "",
): IncidentsCheckpointModel {
  const search = normalizeSearch(searchQuery);
  const allAlerts = data.alerts?.alerts || [];
  const scopedAlerts = allAlerts.filter((alert) =>
    alertMatchesPipeline(alert, pipelineId),
  );
  const alerts = scopedAlerts.filter(
    (alert) => !search || alertSearchText(alert).includes(search),
  );
  const alertGroups = groupAlerts(alerts);
  const critical = scopedAlerts.filter(
    (alert) => alert.severity === "critical",
  ).length;
  const warning = scopedAlerts.filter(
    (alert) => alert.severity === "warning",
  ).length;
  const scopedEvents = [...(data.events?.events || [])]
    .filter((event) => !pipelineId || event.pipelineId === pipelineId)
    .sort((a, b) => b.seq - a.seq)
    .slice(0, 30);
  const events = scopedEvents.filter(
    (event) => !search || eventSearchText(event).includes(search),
  );
  const scopeLabel = incidentScopeLabel(pipelines, pipelineId);
  const summary = data.loaded
    ? `${critical} critical · ${warning} warning · ${pluralize(scopedEvents.length, "recent event")} · ${scopeLabel}`
    : `Loading incident snapshots · ${scopeLabel}`;
  const searchLabel = data.loaded
    ? search
      ? `${pluralize(alertGroups.length, "alert group")} · ${pluralize(events.length, "event")} match "${searchQuery.trim()}"`
      : `${pluralize(alertGroups.length, "alert group")} · ${pluralize(events.length, "event")} visible`
    : "Loading matches";
  const statusLabel = !data.loaded
    ? "Loading"
    : critical > 0
      ? `${critical} critical`
      : warning > 0
        ? `${warning} warning`
        : data.unavailable
          ? "Partial"
          : "Clear";
  const alertLabel = data.loaded
    ? `${critical} critical · ${warning} warning`
    : "Loading alerts";
  const eventLabel = data.loaded
    ? pluralize(scopedEvents.length, "recent event")
    : "Loading events";
  const focusLabel = !data.loaded
    ? "Incident snapshots are loading. Keep the feed below as the source of truth once data arrives."
    : critical > 0
      ? "Critical alerts are active. Start with the highest-severity group below and confirm blast radius."
      : warning > 0
        ? "Warnings are active. Check the matching alert group, then use lifecycle evidence to confirm recovery."
        : search && alertGroups.length + events.length === 0
          ? "No matching incident evidence for this search. Clear the search to return to the full feed."
          : data.unavailable
            ? "Some incident data is stale or unavailable. Verify telemetry before changing outputs."
            : "No active alerts in this scope. Use lifecycle events below for recent context.";
  const nextStep = !data.loaded
    ? "Wait for the snapshot or refresh manually."
    : critical + warning > 0
      ? "Open the affected pipeline from the alert group, then compare telemetry."
      : search
        ? "Clear search or switch scope if you are hunting a different output."
        : "Keep monitoring or jump to Telemetry for counter-level evidence.";

  return {
    alertLabel,
    canOpenTelemetry: true,
    eventLabel,
    focusLabel,
    metrics: [
      {
        label: "Degraded pipelines",
        value: String(data.overview?.degradedPipelines ?? "—"),
      },
      {
        label: "Failed outputs",
        value: String(data.overview?.failedOutputs ?? "—"),
      },
    ],
    nextStep,
    scopeLabel: incidentScopeCardLabel(scopeLabel),
    searchLabel,
    statusLabel,
    statusTone: incidentStatusTone(data, critical, warning),
    summary,
    title: "Incidents",
  };
}

export function renderIncidentsHtml(
  data: IncidentSnapshot,
  pipelines: IncidentPipelineOption[],
  pipelineId: string,
  searchQuery = "",
): string {
  const search = normalizeSearch(searchQuery);
  const allAlerts = data.alerts?.alerts || [];
  const scopedAlerts = allAlerts
    .filter((alert) => alertMatchesPipeline(alert, pipelineId))
    .sort((left, right) => {
      const severity =
        Number(right.severity === "critical") -
        Number(left.severity === "critical");
      return severity || left.title.localeCompare(right.title);
    });
  const alerts = scopedAlerts.filter(
    (alert) => !search || alertSearchText(alert).includes(search),
  );
  const alertGroups = groupAlerts(alerts);
  const critical = scopedAlerts.filter(
    (alert) => alert.severity === "critical",
  ).length;
  const warning = scopedAlerts.filter(
    (alert) => alert.severity === "warning",
  ).length;
  const scopedEvents = [...(data.events?.events || [])]
    .filter((event) => !pipelineId || event.pipelineId === pipelineId)
    .sort((a, b) => b.seq - a.seq)
    .slice(0, 30);
  const events = scopedEvents.filter(
    (event) => !search || eventSearchText(event).includes(search),
  );
  const showAlertGroupToggle =
    !search && alertGroups.length > INCIDENT_ALERT_GROUP_VISIBLE_LIMIT;
  const visibleAlertGroups =
    showAlertGroupToggle && !incidentAlertGroupsExpanded
      ? alertGroups.slice(0, INCIDENT_ALERT_GROUP_VISIBLE_LIMIT)
      : alertGroups;
  const alertGroupCaption = showAlertGroupToggle
    ? `${pluralize(visibleAlertGroups.length, "alert group")} shown of ${alertGroups.length}. Search to isolate an output or show all when auditing the full incident set.`
    : "";
  const showEventToggle = !search && events.length > INCIDENT_EVENT_VISIBLE_LIMIT;
  const visibleEvents =
    showEventToggle && !incidentEventsExpanded
      ? events.slice(0, INCIDENT_EVENT_VISIBLE_LIMIT)
      : events;
  const eventCaption = showEventToggle
    ? `${pluralize(visibleEvents.length, "event")} shown of ${events.length}. Search to isolate lifecycle evidence or show all when reviewing the full timeline.`
    : "";
  const options = [
    `<option value="">All pipelines</option>`,
    ...pipelines.map(
      (pipeline) =>
        `<option value="${escapeHtml(pipeline.id)}"${pipeline.id === pipelineId ? " selected" : ""}>${escapeHtml(pipeline.name || pipeline.id)}</option>`,
    ),
  ].join("");
  const overview = data.overview;
  const availability = !data.loaded
    ? `<div class="alert"><span>Loading incident snapshots…</span></div>`
    : data.unavailable
      ? `<div class="alert alert-warning"><span>Some incident data is temporarily unavailable. Last known data remains visible.</span></div>`
      : "";
  const scopeLabel = incidentScopeLabel(pipelines, pipelineId);
  const summaryText = data.loaded
    ? `${critical} critical · ${warning} warning · ${pluralize(scopedEvents.length, "recent event")} · ${scopeLabel}`
    : `Loading incident snapshots · ${scopeLabel}`;
  const searchSummaryText = data.loaded
    ? search
      ? `${pluralize(alertGroups.length, "alert group")} · ${pluralize(events.length, "event")} match "${searchQuery.trim()}"`
      : `${pluralize(alertGroups.length, "alert group")} · ${pluralize(events.length, "event")} visible`
    : `Loading incident matches · ${scopeLabel}`;

  return `<div class="mx-auto max-w-7xl space-y-4">
    <header class="flex flex-wrap items-end justify-between gap-3">
      <div><h1 class="text-lg font-semibold">Incidents</h1><p class="text-base-content/60 mt-1 text-sm">Current alerts and recent lifecycle evidence from authoritative snapshots.</p></div>
      <div class="flex items-center gap-2"><select id="incidents-pipeline-filter" class="select select-sm" aria-label="Filter incidents by pipeline">${options}</select><button id="incidents-refresh-btn" type="button" class="btn btn-sm btn-outline">Refresh</button></div>
    </header>
    <p id="incidents-route-summary" class="text-base-content/60 text-sm" role="status" aria-live="polite">${escapeHtml(summaryText)}</p>
    <div class="flex flex-wrap items-end gap-3">
      <label class="form-control w-full max-w-md">
        <span class="label-text text-base-content/70">Search incidents and events</span>
        <input id="incidents-search" class="input input-sm input-bordered mt-1" type="search" value="${escapeHtml(searchQuery)}" placeholder="output, pipeline, cause, event…" autocomplete="off" />
      </label>
      <button id="incidents-clear-search-btn" type="button" class="btn btn-sm btn-outline ${search ? "" : "hidden"}">Clear search</button>
      <p id="incidents-search-results-summary" class="text-base-content/60 pb-1 text-sm" role="status" aria-live="polite">${escapeHtml(searchSummaryText)}</p>
    </div>
    ${availability}
    <section class="grid gap-3 sm:grid-cols-2 lg:grid-cols-4" aria-label="Incident rollup">
      <div class="stat bg-base-200 rounded-lg"><div class="stat-title">Critical</div><div class="stat-value text-error text-2xl">${data.alerts ? critical : "—"}</div></div>
      <div class="stat bg-base-200 rounded-lg"><div class="stat-title">Warning</div><div class="stat-value text-warning text-2xl">${data.alerts ? warning : "—"}</div></div>
      <div class="stat bg-base-200 rounded-lg"><div class="stat-title">Degraded pipelines (fleet)</div><div class="stat-value text-2xl">${overview?.degradedPipelines ?? "—"}</div></div>
      <div class="stat bg-base-200 rounded-lg"><div class="stat-title">Failed outputs (fleet)</div><div class="stat-value text-2xl">${overview?.failedOutputs ?? "—"}</div></div>
    </section>
    <div class="grid gap-4 xl:grid-cols-[minmax(0,1.35fr)_minmax(20rem,.65fr)]">
      <section>
        <div class="mb-3 flex flex-wrap items-start justify-between gap-3">
          <div>
            <h2 class="font-semibold">Active alerts</h2>
            ${alertGroupCaption ? `<p class="text-base-content/60 mt-1 text-xs">${escapeHtml(alertGroupCaption)}</p>` : ""}
          </div>
          ${
            showAlertGroupToggle
              ? `<button id="incidents-alerts-toggle" type="button" class="btn btn-xs btn-outline" aria-expanded="${incidentAlertGroupsExpanded ? "true" : "false"}">${incidentAlertGroupsExpanded ? "Show fewer" : `Show all ${alertGroups.length}`}</button>`
              : ""
          }
        </div>
        <div class="space-y-3">${data.loaded && data.alerts && !alertGroups.length ? `<div class="border-base-content/10 bg-base-200 rounded-lg border p-6 text-center text-sm">${search ? `No alert matches for "${escapeHtml(searchQuery.trim())}".` : `No active alerts${pipelineId ? " for this pipeline" : ""}.`}</div>` : visibleAlertGroups.map(renderAlertGroup).join("")}</div>
      </section>
      <section class="border-base-content/10 bg-base-200 self-start rounded-lg border p-4" aria-label="Incident lifecycle events">
        <div class="flex flex-wrap items-start justify-between gap-3">
          <div>
            <h2 class="font-semibold">Recent lifecycle events</h2>
            ${eventCaption ? `<p class="text-base-content/60 mt-1 text-xs">${escapeHtml(eventCaption)}</p>` : ""}
          </div>
          ${
            showEventToggle
              ? `<button id="incidents-events-toggle" type="button" class="btn btn-xs btn-outline" aria-expanded="${incidentEventsExpanded ? "true" : "false"}">${incidentEventsExpanded ? "Show fewer" : `Show all ${events.length}`}</button>`
              : ""
          }
        </div>
        <ul class="mt-2">${data.loaded && data.events && !events.length ? `<li class="py-6 text-center text-sm text-base-content/60">${search ? `No event matches for "${escapeHtml(searchQuery.trim())}".` : "No recent lifecycle events."}</li>` : visibleEvents.map(renderEvent).join("")}</ul>
      </section>
    </div>
  </div>`;
}

function bindIncidentControls(): void {
  const filter = document.getElementById(
    "incidents-pipeline-filter",
  ) as HTMLSelectElement | null;
  filter?.addEventListener("change", () => {
    selectIncidentPipeline(filter.value);
  });
  const search = document.getElementById(
    "incidents-search",
  ) as HTMLInputElement | null;
  search?.addEventListener("input", () => {
    const cursor = search.selectionStart ?? search.value.length;
    incidentSearchQuery = search.value;
    incidentAlertGroupsExpanded = false;
    incidentEventsExpanded = false;
    paintIncidents();
    const nextSearch = document.getElementById(
      "incidents-search",
    ) as HTMLInputElement | null;
    nextSearch?.focus();
    nextSearch?.setSelectionRange(cursor, cursor);
  });
  document
    .getElementById("incidents-clear-search-btn")
    ?.addEventListener("click", () => {
      incidentSearchQuery = "";
      incidentAlertGroupsExpanded = false;
      incidentEventsExpanded = false;
      paintIncidents();
      (
        document.getElementById("incidents-search") as HTMLInputElement | null
      )?.focus();
    });
  document
    .getElementById("incidents-refresh-btn")
    ?.addEventListener("click", () => {
      void refreshIncidents(true);
    });
  document
    .getElementById("incidents-alerts-toggle")
    ?.addEventListener("click", () => {
      incidentAlertGroupsExpanded = !incidentAlertGroupsExpanded;
      paintIncidents();
    });
  document
    .getElementById("incidents-events-toggle")
    ?.addEventListener("click", () => {
      incidentEventsExpanded = !incidentEventsExpanded;
      paintIncidents();
    });
  document
    .querySelectorAll<HTMLElement>("[data-open-incident-pipeline]")
    .forEach((button) => {
      button.addEventListener("click", () => {
        const pipelineId = button.dataset.openIncidentPipeline;
        if (pipelineId) viewOptions?.navigateToPipeline(pipelineId);
      });
    });
}

export function selectIncidentPipeline(pipelineId: string): void {
  if (pipelineId === selectedPipelineId) return;
  selectedPipelineId = pipelineId;
  incidentAlertGroupsExpanded = false;
  incidentEventsExpanded = false;
  // Lifecycle events are scope-specific. Do not paint the previous scope as a
  // successful empty result while the replacement snapshot is in flight.
  snapshot = { ...snapshot, events: null, loaded: false, unavailable: false };
  paintIncidents();
  void refreshIncidents(true);
}

function paintIncidents(): void {
  const root = document.getElementById("incidents-mode-content");
  if (!root || !viewOptions) return;
  incidentsCheckpointCallback?.(
    buildIncidentsCheckpointModel(
      snapshot,
      viewOptions.pipelines,
      selectedPipelineId,
      incidentSearchQuery,
    ),
  );
  root.innerHTML = renderIncidentsHtml(
    snapshot,
    viewOptions.pipelines,
    selectedPipelineId,
    incidentSearchQuery,
  );
  bindIncidentControls();
}

export async function refreshIncidents(force = false): Promise<void> {
  if (!viewOptions?.active || document.hidden) return;
  if (!force && Date.now() - lastFetchedAt < INCIDENT_REFRESH_MS) return;
  const scope = selectedPipelineId;
  const existing = inFlightByScope.get(scope);
  if (existing?.sequence === requestSequence) return existing.promise;
  const sequence = ++requestSequence;
  const pipelineAtRequest = selectedPipelineId;
  const request = (async () => {
    const [overview, alerts, events] = await Promise.all([
      getOverview(),
      getAggregateAlerts(),
      getLifecycleEvents({
        pipelineId: pipelineAtRequest || null,
        limit: INCIDENT_EVENT_LIMIT,
      }),
    ]);
    if (
      sequence !== requestSequence ||
      pipelineAtRequest !== selectedPipelineId
    )
      return;
    snapshot = {
      overview: overview ?? snapshot.overview,
      alerts: alerts ?? snapshot.alerts,
      events: events ?? snapshot.events,
      loaded: true,
      unavailable: overview === null || alerts === null || events === null,
    };
    lastFetchedAt = Date.now();
    paintIncidents();
  })().finally(() => {
    if (inFlightByScope.get(scope)?.promise === request) {
      inFlightByScope.delete(scope);
    }
  });
  inFlightByScope.set(scope, { sequence, promise: request });
  return request;
}

export function renderIncidentsMode(options: IncidentsViewOptions): void {
  viewOptions = options;
  if (
    !options.pipelines.some((pipeline) => pipeline.id === selectedPipelineId)
  ) {
    selectedPipelineId = "";
  }
  if (!options.active) {
    incidentsCheckpointCallback?.(null);
    return;
  }
  paintIncidents();
  void refreshIncidents();
}
