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

export function renderIncidentsHtml(
  data: IncidentSnapshot,
  pipelines: IncidentPipelineOption[],
  pipelineId: string,
): string {
  const allAlerts = data.alerts?.alerts || [];
  const alerts = allAlerts
    .filter((alert) => alertMatchesPipeline(alert, pipelineId))
    .sort((left, right) => {
      const severity =
        Number(right.severity === "critical") -
        Number(left.severity === "critical");
      return severity || left.title.localeCompare(right.title);
    });
  const alertGroups = groupAlerts(alerts);
  const critical = alerts.filter(
    (alert) => alert.severity === "critical",
  ).length;
  const warning = alerts.filter((alert) => alert.severity === "warning").length;
  const events = [...(data.events?.events || [])]
    .filter((event) => !pipelineId || event.pipelineId === pipelineId)
    .sort((a, b) => b.seq - a.seq)
    .slice(0, 30);
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

  return `<div class="mx-auto max-w-7xl space-y-4">
    <header class="flex flex-wrap items-end justify-between gap-3">
      <div><h1 class="text-lg font-semibold">Incidents</h1><p class="text-base-content/60 mt-1 text-sm">Current alerts and recent lifecycle evidence from authoritative snapshots.</p></div>
      <div class="flex items-center gap-2"><select id="incidents-pipeline-filter" class="select select-sm" aria-label="Filter incidents by pipeline">${options}</select><button id="incidents-refresh-btn" type="button" class="btn btn-sm btn-outline">Refresh</button></div>
    </header>
    ${availability}
    <section class="grid gap-3 sm:grid-cols-2 lg:grid-cols-4" aria-label="Incident rollup">
      <div class="stat bg-base-200 rounded-lg"><div class="stat-title">Critical</div><div class="stat-value text-error text-2xl">${data.alerts ? critical : "—"}</div></div>
      <div class="stat bg-base-200 rounded-lg"><div class="stat-title">Warning</div><div class="stat-value text-warning text-2xl">${data.alerts ? warning : "—"}</div></div>
      <div class="stat bg-base-200 rounded-lg"><div class="stat-title">Degraded pipelines (fleet)</div><div class="stat-value text-2xl">${overview?.degradedPipelines ?? "—"}</div></div>
      <div class="stat bg-base-200 rounded-lg"><div class="stat-title">Failed outputs (fleet)</div><div class="stat-value text-2xl">${overview?.failedOutputs ?? "—"}</div></div>
    </section>
    <div class="grid gap-4 xl:grid-cols-[minmax(0,1.35fr)_minmax(20rem,.65fr)]">
      <section><h2 class="mb-3 font-semibold">Active alerts</h2><div class="space-y-3">${data.loaded && data.alerts && !alerts.length ? `<div class="border-base-content/10 bg-base-200 rounded-lg border p-6 text-center text-sm">No active alerts${pipelineId ? " for this pipeline" : ""}.</div>` : alertGroups.map(renderAlertGroup).join("")}</div></section>
      <section class="border-base-content/10 bg-base-200 self-start rounded-lg border p-4"><h2 class="font-semibold">Recent lifecycle events</h2><ul class="mt-2">${data.loaded && data.events && !events.length ? `<li class="py-6 text-center text-sm text-base-content/60">No recent lifecycle events.</li>` : events.map(renderEvent).join("")}</ul></section>
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
  document
    .getElementById("incidents-refresh-btn")
    ?.addEventListener("click", () => {
      void refreshIncidents(true);
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
  // Lifecycle events are scope-specific. Do not paint the previous scope as a
  // successful empty result while the replacement snapshot is in flight.
  snapshot = { ...snapshot, events: null, loaded: false, unavailable: false };
  paintIncidents();
  void refreshIncidents(true);
}

function paintIncidents(): void {
  const root = document.getElementById("incidents-mode-content");
  if (!root || !viewOptions) return;
  root.innerHTML = renderIncidentsHtml(
    snapshot,
    viewOptions.pipelines,
    selectedPipelineId,
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
  if (!options.active) return;
  paintIncidents();
  void refreshIncidents();
}
