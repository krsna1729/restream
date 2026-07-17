import {
  getEngineSbomEndpoint,
  getEngineStatus,
  getRestreamHistory,
} from "../core/api.js";
import { createManagedLogStream } from "../core/log-stream.js";
import { withBasePath } from "../core/base-path.js";
import { redirectToLogin } from "../core/auth-redirect.js";
import {
  copyText,
  escapeHtml,
  escapeRedactedHtml,
  showCopiedNotification,
  showErrorAlert,
} from "../core/utils.js";
import { handleDashboardRuntimeLifecycleLog } from "./dashboard.js";
import { updateRestreamProcessIndicatorFromLog } from "./restream-process-indicator.js";
import type { AppLogRow } from "../types.js";
import type { StatusCheckpointModel } from "./status-view-model.js";

interface StatusData {
  restream: {
    version?: string;
    commit?: string;
    nativeBuildId?: string;
  };
  toolchain?: {
    rustc?: string;
    target?: string;
    llvm?: string;
    gccRuntime?: string;
  };
  nativeLibraries?: {
    ffmpeg?: {
      version?: string;
      license?: string;
      x86Assembly?: boolean;
    };
    srt?: {
      version?: string;
      buildVersion?: string;
      license?: string;
      bondingAvailable?: boolean;
    };
    mbedtls?: {
      version?: string;
      buildVersion?: string;
      license?: string;
    };
    sqlite?: {
      version?: string;
      sourceId?: string;
      license?: string;
    };
    x264?: {
      version?: string;
      license?: string;
      versionSource?: string;
    };
    x265?: {
      version?: string;
      license?: string;
      versionSource?: string;
    };
  };
  sbom?: {
    endpoint?: string;
    componentCount?: number;
    rustComponentCount?: number;
    nativeComponentCount?: number;
    nativeComponents?: string[];
    licensesIncluded?: boolean;
  };
  os?: {
    platform?: string;
    arch?: string;
    hostname?: string;
    kernelVersion?: string | null;
    uptime?: number;
    totalMem?: number;
    cpu?: {
      modelName?: string | null;
      logicalCpus?: number;
      physicalCores?: number | null;
      threadsPerCore?: number | null;
      virtualization?: string | null;
      hypervisorDetected?: boolean;
      hypervisorVendor?: string | null;
      flags?: string[];
    };
  };
}

const STATUS_PROCESS_LOG_LIMIT = 80;
const STATUS_PROCESS_LOG_VISIBLE_LIMIT = 20;
const STATUS_ACTIVITY_LIMIT = 12;
const STATUS_SECTION_NAV = [
  {
    id: "status-build-section",
    label: "Build",
    ariaLabel: "Jump to build status",
  },
  {
    id: "status-system-section",
    label: "System",
    ariaLabel: "Jump to system status",
  },
  {
    id: "status-toolchain-section",
    label: "Toolchain",
    ariaLabel: "Jump to toolchain details",
  },
  {
    id: "status-native-section",
    label: "Libraries",
    ariaLabel: "Jump to native library details",
  },
  {
    id: "status-sbom-section",
    label: "SBOM",
    ariaLabel: "Jump to SBOM details",
  },
  {
    id: "status-activity-section",
    label: "Activity",
    ariaLabel: "Jump to recent activity",
  },
  {
    id: "status-log-section",
    label: "Logs",
    ariaLabel: "Jump to process logs",
  },
] as const;
let statusDataSnapshot: StatusData | null = null;
let statusProcessLogs: AppLogRow[] = [];
const statusStream = createManagedLogStream();
let statusStreamActive = false;
let statusStreamLastEventId: number | null = null;
let statusLogSearchQuery = "";
let statusProcessLogExpanded = false;
let statusExportActionsExpanded = false;
const statusAdvancedSectionsExpanded = new Set<string>();
let statusCheckpointCallback:
  | ((model: StatusCheckpointModel | null) => void)
  | null = null;

export function configureStatusCheckpointPresentation(options: {
  readonly onPresentation?: (model: StatusCheckpointModel | null) => void;
}): void {
  statusCheckpointCallback = options.onPresentation ?? null;
  if (!statusCheckpointCallback) return;
  statusCheckpointCallback(buildStatusCheckpointModel());
}

function syncProcessIndicatorFromLogs(logs: AppLogRow[]): void {
  for (const log of logs) {
    updateRestreamProcessIndicatorFromLog(log);
  }
}

function latestStatusProcessLog(logs: AppLogRow[]): AppLogRow | null {
  let latest: AppLogRow | null = null;
  let latestId = Number.NEGATIVE_INFINITY;
  for (const log of logs) {
    const id = Number(log?.id);
    if (!Number.isFinite(id) || id <= 0) continue;
    if (id > latestId) {
      latest = log;
      latestId = id;
    }
  }
  return latest;
}

function valueOrDash(value: unknown): string {
  if (value === null || value === undefined || value === "") return "--";
  if (typeof value === "boolean") return value ? "yes" : "no";
  return String(value);
}

function row(label: string, value: unknown): string {
  return `<tr>
        <td class="text-base-content/65 py-1.5 pr-4 align-top font-medium whitespace-nowrap">${escapeHtml(label)}</td>
        <td class="py-1.5 align-top font-mono text-sm break-all">${escapeHtml(valueOrDash(value))}</td>
    </tr>`;
}

function formatBytes(value: unknown): string {
  const bytes = Number(value);
  if (!Number.isFinite(bytes) || bytes < 0) return "--";
  if (bytes < 1024) return `${bytes.toFixed(0)} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  if (bytes < 1024 * 1024 * 1024)
    return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GiB`;
}

function formatThreadsPerCore(value: unknown): string {
  const n = Number(value);
  if (!Number.isFinite(n) || n <= 0) return "--";
  return Number.isInteger(n) ? n.toFixed(0) : n.toFixed(1);
}

type StatusCpu = NonNullable<StatusData["os"]>["cpu"];

function formatCpuCapacity(cpu: StatusCpu): string {
  if (!cpu) return "--";
  const logical = Number(cpu.logicalCpus);
  const parts = [];
  if (Number.isFinite(logical) && logical > 0) {
    parts.push(`${logical.toFixed(0)} logical`);
  }
  if (cpu.physicalCores) {
    parts.push(`${cpu.physicalCores} physical`);
  }
  const threads = formatThreadsPerCore(cpu.threadsPerCore);
  if (threads !== "--") {
    parts.push(`${threads} threads/core`);
  }
  return parts.length ? parts.join(" / ") : "--";
}

function formatFlags(value: unknown): string {
  if (!Array.isArray(value) || value.length === 0) return "--";
  return value.map((flag) => String(flag)).join(", ");
}

function formatList(value: unknown): string {
  if (!Array.isArray(value) || value.length === 0) return "--";
  return value.map((item) => String(item)).join(", ");
}

function formatVirtualization(cpu: StatusCpu): string {
  if (!cpu) return "--";
  const parts = [];
  if (cpu.virtualization) parts.push(cpu.virtualization);
  if (cpu.hypervisorDetected) {
    parts.push(
      cpu.hypervisorVendor
        ? `${cpu.hypervisorVendor} hypervisor`
        : "hypervisor detected",
    );
  }
  return parts.length ? parts.join(" / ") : "bare metal or not exposed";
}

function versionRows(
  label: string,
  runtimeVersion: unknown,
  buildVersion?: unknown,
): string {
  const rows = [row(`${label} Version`, runtimeVersion)];
  const runtime = valueOrDash(runtimeVersion);
  const build = valueOrDash(buildVersion);
  if (build !== "--" && build !== runtime) {
    rows.push(row(`${label} Build-Time Version`, buildVersion));
  }
  return rows.join("");
}

function formatUptime(value: unknown): string {
  const seconds = Number(value);
  if (!Number.isFinite(seconds) || seconds < 0) return "--";
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const parts = [];
  if (days) parts.push(`${days}d`);
  if (hours || days) parts.push(`${hours}h`);
  parts.push(`${minutes}m`);
  return parts.join(" ");
}

function section(id: string, title: string, rows: string): string {
  return `<section id="${escapeHtml(id)}" class="scroll-mt-24">
        <div class="dashboard-kicker mb-2">${escapeHtml(title)}</div>
        <div class="overflow-x-auto" role="region" aria-label="${escapeHtml(title)} details" tabindex="0">
            <table class="w-full min-w-[36rem] table-fixed text-sm">
                <colgroup>
                    <col class="w-48 sm:w-56" />
                    <col />
                </colgroup>
                <tbody>${rows}</tbody>
            </table>
        </div>
    </section>`;
}

function advancedSectionActionLabel(
  action: "Show" | "Hide",
  title: string,
): string {
  const detailName =
    title === "Native Libraries"
      ? "native library"
      : title === "SBOM"
        ? "SBOM"
        : title.toLowerCase();
  return `${action} ${detailName} details`;
}

function statusV2Active(): boolean {
  const toggle = document.getElementById("dashboard-ui-v2-toggle");
  if (toggle instanceof HTMLInputElement && toggle.checked) return true;
  try {
    return new URLSearchParams(window.location.search).get("ui") === "v2";
  } catch (_err) {
    return false;
  }
}

function advancedSection(
  id: string,
  title: string,
  summary: string,
  rows: string,
): string {
  const expanded = !statusV2Active() || statusAdvancedSectionsExpanded.has(id);
  if (expanded) {
    const hideLabel = advancedSectionActionLabel("Hide", title);
    return `${section(id, title, rows)}
      ${
        statusV2Active()
          ? `<button type="button" class="btn btn-xs btn-outline mt-2" data-status-advanced-section="${escapeHtml(id)}" aria-label="${escapeHtml(hideLabel)}" aria-expanded="true">Hide ${escapeHtml(title)} details</button>`
          : ""
      }`;
  }
  const showLabel = advancedSectionActionLabel("Show", title);
  return `<section id="${escapeHtml(id)}" class="scroll-mt-24">
        <div class="border-base-content/10 bg-base-100/60 rounded-lg border px-3 py-2">
            <div class="flex flex-wrap items-center justify-between gap-2">
                <div class="dashboard-kicker">${escapeHtml(title)}</div>
                <button type="button" class="btn btn-xs btn-outline" data-status-advanced-section="${escapeHtml(id)}" aria-label="${escapeHtml(showLabel)}" aria-expanded="false">Show ${escapeHtml(title)} details</button>
            </div>
            <p class="dashboard-muted mt-1 text-sm">${escapeHtml(summary)}</p>
        </div>
    </section>`;
}

function statusExportActionsHtml(): string {
  const actions = `
            <div class="flex flex-wrap gap-2">
                <button type="button" class="btn btn-sm btn-outline" id="download-status-btn">Download status report</button>
                <button type="button" class="btn btn-sm btn-outline" id="copy-status-btn">Copy status report</button>
                <button type="button" class="btn btn-sm btn-outline" id="download-sbom-btn">Download SBOM file</button>
                <button type="button" class="btn btn-sm btn-outline" id="copy-sbom-btn">Copy SBOM file</button>
            </div>`;
  if (!statusV2Active()) return actions;
  return `
        <section class="border-base-content/10 bg-base-100 rounded-2xl border p-4 shadow-sm" aria-label="Status export actions">
            <div class="flex flex-wrap items-start justify-between gap-3">
                <div>
                    <div class="text-sm font-semibold">Export actions</div>
                    <p class="dashboard-muted mt-1 text-sm">Download or copy runtime/SBOM evidence only when preparing an audit bundle.</p>
                </div>
                <button type="button" class="btn btn-sm btn-outline" id="status-export-actions-toggle" aria-label="${statusExportActionsExpanded ? "Hide status export actions" : "Show status export actions"}" aria-expanded="${statusExportActionsExpanded ? "true" : "false"}">
                    ${statusExportActionsExpanded ? "Hide export actions" : "Show export actions"}
                </button>
            </div>
            ${statusExportActionsExpanded ? `<div class="mt-3">${actions}</div>` : ""}
        </section>`;
}

function statusQuickNavHtml(): string {
  return `<nav class="dashboard-nav-strip" aria-label="Status sections">
      ${STATUS_SECTION_NAV.map(
        (item) =>
          `<a class="btn btn-sm btn-ghost" href="#${escapeHtml(item.id)}" aria-label="${escapeHtml(item.ariaLabel)}">${escapeHtml(item.label)}</a>`,
      ).join("")}
    </nav>`;
}

function formatLogTime(ts: string | null | undefined): string {
  if (!ts) return "--";
  const d = new Date(ts);
  if (Number.isNaN(d.getTime())) return ts;
  return d.toLocaleString();
}

function normalizeEventType(log: AppLogRow | null | undefined): string {
  return String(log?.eventType || "")
    .trim()
    .toLowerCase();
}

function classifyRestreamActivity(log: AppLogRow): {
  label: string;
  badgeClass: string;
} {
  const eventType = normalizeEventType(log);
  const target = String(log?.target || "");
  const message = String(log?.message || "");
  const level = String(log?.level || "").toUpperCase();

  if (eventType === "restream.http.ready") {
    return { label: "API Ready", badgeClass: "badge-success" };
  }
  if (eventType === "restream.shutdown.requested") {
    return { label: "Shutdown Requested", badgeClass: "badge-warning" };
  }
  if (eventType === "restream.shutdown.started") {
    return { label: "Stopping", badgeClass: "badge-warning" };
  }
  if (eventType === "restream.shutdown.completed") {
    return { label: "Stopped", badgeClass: "badge-stopped" };
  }
  if (/task exited unexpectedly/i.test(message)) {
    return { label: "Server Task Exit", badgeClass: "badge-error" };
  }
  if (/dashboard api server listening/i.test(message)) {
    return { label: "API Ready", badgeClass: "badge-success" };
  }
  if (/server listening/i.test(message)) {
    return { label: "Listener Ready", badgeClass: "badge-success" };
  }
  if (/raised file descriptor limit/i.test(message)) {
    return { label: "Limits Raised", badgeClass: "badge-info" };
  }
  if (target.includes("profiles") && /loaded|updated/i.test(message)) {
    return { label: "Profiles Updated", badgeClass: "badge-secondary" };
  }
  if (level === "ERROR") {
    return { label: "Error", badgeClass: "badge-error" };
  }
  if (level === "WARN") {
    return { label: "Warning", badgeClass: "badge-warning" };
  }
  return { label: "Process", badgeClass: "badge-ghost" };
}

function isNotableRestreamActivity(log: AppLogRow): boolean {
  const eventType = normalizeEventType(log);
  const message = String(log?.message || "");
  const level = String(log?.level || "").toUpperCase();

  if (eventType.startsWith("restream.")) return true;
  if (level === "WARN" || level === "ERROR") return true;
  return /listening|shutdown|exited unexpectedly|raised file descriptor limit|loaded profiles|updated profiles/i.test(
    message,
  );
}

function normalizeStatusSearch(value: string): string {
  return value.trim().toLowerCase();
}

function statusLogSearchText(log: AppLogRow): string {
  return [
    log.id,
    log.ts,
    log.level,
    log.target,
    log.message,
    log.eventType,
    log.eventClass,
  ]
    .filter((value) => value !== null && value !== undefined && value !== "")
    .join(" ")
    .toLowerCase();
}

function logMatchesSearch(log: AppLogRow, search: string): boolean {
  return !search || statusLogSearchText(log).includes(search);
}

function statusLogSearchSummaryText(
  activityCount: number,
  logCount: number,
  query: string,
): string {
  const trimmed = query.trim();
  if (!trimmed) {
    return `${pluralize(activityCount, "activity", "activities")} · ${pluralize(logCount, "process log")} visible`;
  }
  return `${pluralize(activityCount, "activity", "activities")} · ${pluralize(logCount, "process log")} match "${trimmed}"`;
}

function statusNoResultText(kind: string, query: string): string {
  const trimmed = query.trim();
  return trimmed
    ? `No ${kind} match "${trimmed}". Clear search to return to the full status view.`
    : `No ${kind} available yet.`;
}

function renderRestreamActivity(
  logs: AppLogRow[],
  search: string,
  query: string,
): string {
  const items = logs
    .filter(isNotableRestreamActivity)
    .filter((log) => logMatchesSearch(log, search))
    .slice(0, STATUS_ACTIVITY_LIMIT);
  if (items.length === 0) {
    return `<section id="status-activity-section" class="dashboard-section scroll-mt-24 p-5">
            <h2 class="dashboard-section-title mb-3">Recent Activity</h2>
            <p class="dashboard-muted">${escapeHtml(search ? statusNoResultText("activity entries", query) : "No unscoped restream activity has been recorded yet.")}</p>
        </section>`;
  }

  const rows = items
    .map((log) => {
      const event = classifyRestreamActivity(log);
      return `<div class="dashboard-card p-3">
                <div class="flex items-center justify-between gap-3">
                    <span class="badge badge-sm ${event.badgeClass}">${escapeHtml(event.label)}</span>
                    <span class="text-xs opacity-70">${escapeHtml(formatLogTime(log.ts))}</span>
                </div>
                <pre class="mt-2 whitespace-pre-wrap break-words text-xs">${escapeRedactedHtml(log.message || "", true)}</pre>
                <div class="text-base-content/55 mt-2 truncate font-mono text-[11px]">${escapeHtml(log.target || "--")}</div>
            </div>`;
    })
    .join("");

  return `<section id="status-activity-section" class="dashboard-section scroll-mt-24 p-5">
        <div class="mb-3">
            <h2 class="dashboard-section-title">Recent Activity</h2>
            <p class="dashboard-subtitle">Restream-wide events that are not tied to a specific pipeline or output.</p>
        </div>
        <div class="space-y-2">${rows}</div>
    </section>`;
}

function renderProcessLog(
  logs: AppLogRow[],
  search: string,
  query: string,
): string {
  const items = Array.isArray(logs)
    ? logs.filter((log) => logMatchesSearch(log, search))
    : [];
  if (!items.length) {
    return `<section id="status-log-section" class="dashboard-section scroll-mt-24 p-5">
            <h2 class="dashboard-section-title mb-3">Process Log</h2>
            <p class="dashboard-muted">${escapeHtml(search ? statusNoResultText("process log entries", query) : "No unscoped process log entries are available yet.")}</p>
        </section>`;
  }

  const showToggle =
    !search && items.length > STATUS_PROCESS_LOG_VISIBLE_LIMIT;
  const visibleItems =
    showToggle && !statusProcessLogExpanded
      ? items.slice(0, STATUS_PROCESS_LOG_VISIBLE_LIMIT)
      : items;
  const caption = showToggle
    ? `${pluralize(visibleItems.length, "process log")} shown of ${items.length}. Search to isolate a target or show all when auditing the full process history.`
    : "";

  const rows = visibleItems
    .map(
      (
        log,
      ) => `<div class="dashboard-card p-3">
                <div class="mb-2 flex flex-wrap items-center gap-2 text-[11px]">
                    <span class="badge badge-sm ${
                      String(log.level || "").toUpperCase() === "ERROR"
                        ? "badge-error"
                        : String(log.level || "").toUpperCase() === "WARN"
                          ? "badge-warning"
                          : "badge-ghost"
                    }">${escapeHtml(log.level || "INFO")}</span>
                    <span class="opacity-70">${escapeHtml(formatLogTime(log.ts))}</span>
                    <span class="text-base-content/55 truncate font-mono">${escapeHtml(log.target || "--")}</span>
                </div>
                <pre class="whitespace-pre-wrap break-words text-xs">${escapeRedactedHtml(log.message || "", true)}</pre>
            </div>`,
    )
    .join("");

  return `<section id="status-log-section" class="dashboard-section scroll-mt-24 p-5">
        <div class="mb-3 flex flex-wrap items-start justify-between gap-3">
            <div>
                <h2 class="dashboard-section-title">Process Log</h2>
                <p class="dashboard-subtitle">Latest restream process logs outside pipeline and output scope.</p>
                ${caption ? `<p class="text-base-content/60 mt-1 text-xs">${escapeHtml(caption)}</p>` : ""}
            </div>
            ${
              showToggle
                ? `<button id="status-log-toggle" type="button" class="btn btn-xs btn-outline" aria-label="${statusProcessLogExpanded ? "Show fewer process logs" : `Show all ${items.length} process logs`}" aria-expanded="${statusProcessLogExpanded ? "true" : "false"}">${statusProcessLogExpanded ? "Show fewer" : `Show all ${items.length}`}</button>`
                : ""
            }
        </div>
        <div class="max-h-[32rem] space-y-2 overflow-y-auto pr-1" role="region" aria-label="Process log entries" tabindex="0">${rows}</div>
    </section>`;
}

function pluralize(count: number, singular: string, plural = `${singular}s`): string {
  return `${count} ${count === 1 ? singular : plural}`;
}

function statusSummaryText(data: StatusData, logs: AppLogRow[]): string {
  const version = valueOrDash(data.restream?.version);
  const commit = valueOrDash(data.restream?.commit);
  const activityCount = logs.filter(isNotableRestreamActivity).length;
  return [
    version === "--" ? "Status loaded" : `Status loaded for ${version}`,
    commit === "--" ? null : `commit ${commit}`,
    pluralize(logs.length, "process log"),
    pluralize(activityCount, "notable activity", "notable activities"),
  ]
    .filter(Boolean)
    .join(" · ");
}

function statusCheckpointTone(
  data: StatusData | null,
  logs: AppLogRow[],
): StatusCheckpointModel["statusTone"] {
  if (!data) return "neutral";
  if (logs.some((log) => String(log.level || "").toUpperCase() === "ERROR")) {
    return "error";
  }
  if (logs.some((log) => String(log.level || "").toUpperCase() === "WARN")) {
    return "warning";
  }
  return "success";
}

function buildStatusCheckpointModel(): StatusCheckpointModel {
  const data = statusDataSnapshot;
  const logs = Array.isArray(statusProcessLogs) ? statusProcessLogs : [];
  const search = normalizeStatusSearch(statusLogSearchQuery);
  const visibleProcessLogs = logs.filter((log) => logMatchesSearch(log, search));
  const visibleActivityLogs = logs
    .filter(isNotableRestreamActivity)
    .filter((log) => logMatchesSearch(log, search));
  const activityCount = logs.filter(isNotableRestreamActivity).length;
  const summary = data
    ? statusSummaryText(data, logs)
    : "Loading runtime status";
  const statusLabel = data
    ? statusCheckpointTone(data, logs) === "error"
      ? "Errors"
      : statusCheckpointTone(data, logs) === "warning"
        ? "Warnings"
        : "Loaded"
    : "Loading";
  const searchLabel = data
    ? statusLogSearchSummaryText(
        visibleActivityLogs.length,
        visibleProcessLogs.length,
        statusLogSearchQuery,
      )
    : "Loading matches";
  const focusLabel = !data
    ? "Runtime status is loading. Build, system, and process-log details will appear below once ready."
    : search && visibleActivityLogs.length + visibleProcessLogs.length === 0
      ? "No process activity matches this search. Clear the filter to return to the full status view."
      : statusCheckpointTone(data, logs) === "error"
        ? "Errors are present in the process log. Start with the latest error entry below, then compare Telemetry."
        : statusCheckpointTone(data, logs) === "warning"
          ? "Warnings are present in the process log. Review recent activity before changing runtime configuration."
          : "Runtime status is loaded. Use the detailed build, system, and process-log sections below for audit depth.";
  const nextStep = !data
    ? "Wait for status to load or refresh manually."
    : statusCheckpointTone(data, logs) === "success"
      ? "Search logs when investigating a specific target or open Telemetry for counters."
      : "Open Telemetry to compare process health with live counters.";

  return {
    activityLabel: data
      ? pluralize(activityCount, "notable activity", "notable activities")
      : "Loading activity",
    buildLabel: data
      ? `${valueOrDash(data.restream?.version)} · ${valueOrDash(data.restream?.commit)}`
      : "Loading build",
    canOpenTelemetry: true,
    focusLabel,
    logLabel: data ? pluralize(logs.length, "process log") : "Loading logs",
    metrics: [
      {
        label: "SBOM components",
        value: String(data?.sbom?.componentCount ?? "—"),
      },
      {
        label: "Uptime",
        value: data ? formatUptime(data.os?.uptime) : "—",
      },
    ],
    nextStep,
    searchLabel,
    statusLabel,
    statusTone: statusCheckpointTone(data, logs),
    summary,
    title: "Status",
  };
}

function statusLogKey(log: AppLogRow | null | undefined): string {
  const id = Number(log?.id);
  if (Number.isFinite(id) && id > 0) return `id:${id}`;
  return `msg:${String(log?.ts || "")}:${String(log?.target || "")}:${String(log?.message || "")}`;
}

function mergeStatusProcessLogs(logs: AppLogRow[]): void {
  const merged = new Map<string, AppLogRow>();
  for (const log of Array.isArray(statusProcessLogs) ? statusProcessLogs : []) {
    merged.set(statusLogKey(log), log);
  }
  for (const log of Array.isArray(logs) ? logs : []) {
    merged.set(statusLogKey(log), log);
  }
  statusProcessLogs = [...merged.values()]
    .sort((a, b) => Date.parse(b.ts || "") - Date.parse(a.ts || ""))
    .slice(0, STATUS_PROCESS_LOG_LIMIT);
}

function latestStatusProcessLogId(): number | null {
  const ids = statusProcessLogs
    .map((log) => Number(log?.id))
    .filter((id) => Number.isFinite(id) && id > 0);
  return ids.length > 0 ? Math.max(...ids) : null;
}

function rememberStatusProcessLogId(log: AppLogRow | null | undefined): void {
  const id = Number(log?.id);
  if (Number.isFinite(id) && id > 0) {
    statusStreamLastEventId = Math.max(statusStreamLastEventId || 0, id);
  }
}

function timestampForFilename(): string {
  return new Date()
    .toISOString()
    .replace(/[:.]/g, "-")
    .replace("T", "_")
    .slice(0, 19);
}

function downloadJson(filename: string, data: unknown): void {
  const blob = new Blob([`${JSON.stringify(data, null, 2)}\n`], {
    type: "application/json",
  });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  a.click();
  URL.revokeObjectURL(url);
}

async function fetchJson(endpoint: string): Promise<unknown | null> {
  try {
    const response = await fetch(withBasePath(endpoint));
    if (response.status === 401) {
      redirectToLogin();
      return null;
    }
    if (!response.ok) {
      showErrorAlert(`Request failed with ${response.status}`);
      return null;
    }
    return await response.json();
  } catch (err) {
    showErrorAlert(`Request failed: ${err}`);
    return null;
  }
}

async function copyJson(data: unknown): Promise<void> {
  if (await copyText(`${JSON.stringify(data, null, 2)}\n`))
    showCopiedNotification();
}

function bindActions(status: StatusData, sbomEndpoint: string): void {
  const search = document.getElementById(
    "status-log-search",
  ) as HTMLInputElement | null;
  search?.addEventListener("input", () => {
    const cursor = search.selectionStart ?? search.value.length;
    statusLogSearchQuery = search.value;
    statusProcessLogExpanded = false;
    renderStatusSnapshot();
    const nextSearch = document.getElementById(
      "status-log-search",
    ) as HTMLInputElement | null;
    nextSearch?.focus();
    nextSearch?.setSelectionRange(cursor, cursor);
  });
  document
    .getElementById("status-clear-search-btn")
    ?.addEventListener("click", () => {
      statusLogSearchQuery = "";
      statusProcessLogExpanded = false;
      renderStatusSnapshot();
      (
        document.getElementById("status-log-search") as HTMLInputElement | null
      )?.focus();
    });
  document.getElementById("status-log-toggle")?.addEventListener("click", () => {
    statusProcessLogExpanded = !statusProcessLogExpanded;
    renderStatusSnapshot();
  });
  document
    .getElementById("status-export-actions-toggle")
    ?.addEventListener("click", () => {
      statusExportActionsExpanded = !statusExportActionsExpanded;
      renderStatusSnapshot();
      document.getElementById("status-export-actions-toggle")?.focus();
    });
  document
    .querySelectorAll<HTMLButtonElement>("[data-status-advanced-section]")
    .forEach((button) => {
      button.addEventListener("click", () => {
        const id = button.dataset.statusAdvancedSection;
        if (!id) return;
        if (statusAdvancedSectionsExpanded.has(id)) {
          statusAdvancedSectionsExpanded.delete(id);
        } else {
          statusAdvancedSectionsExpanded.add(id);
        }
        renderStatusSnapshot();
        Array.from(
          document.querySelectorAll<HTMLButtonElement>(
            "[data-status-advanced-section]",
          ),
        )
          .find((nextButton) => nextButton.dataset.statusAdvancedSection === id)
          ?.focus();
      });
    });
  document
    .getElementById("download-status-btn")
    ?.addEventListener("click", () => {
      downloadJson(`restream-status-${timestampForFilename()}.json`, status);
    });
  document
    .getElementById("copy-status-btn")
    ?.addEventListener("click", () => void copyJson(status));
  document
    .getElementById("download-sbom-btn")
    ?.addEventListener("click", async () => {
      const sbom = await fetchJson(sbomEndpoint);
      if (sbom)
        downloadJson(`restream-sbom-${timestampForFilename()}.cdx.json`, sbom);
    });
  document
    .getElementById("copy-sbom-btn")
    ?.addEventListener("click", async () => {
      const sbom = await fetchJson(sbomEndpoint);
      if (sbom) await copyJson(sbom);
    });
}

function closeStatusStream(): void {
  statusStreamLastEventId =
    statusStream.getLastEventId() ?? statusStreamLastEventId;
  statusStream.close();
}

function statusStreamingEnabled(): boolean {
  return statusStreamActive && !document.hidden;
}

function renderStatusSnapshot(): void {
  const container = document.getElementById("status-versions");
  if (!container || !statusDataSnapshot) return;
  statusCheckpointCallback?.(buildStatusCheckpointModel());

  const data = statusDataSnapshot;
  const processLogs = statusProcessLogs;
  const ffmpeg = data.nativeLibraries?.ffmpeg;
  const srt = data.nativeLibraries?.srt;
  const mbedtls = data.nativeLibraries?.mbedtls;
  const sqlite = data.nativeLibraries?.sqlite;
  const sbomEndpoint = getEngineSbomEndpoint(data);
  const search = normalizeStatusSearch(statusLogSearchQuery);
  const visibleProcessLogs = processLogs.filter((log) =>
    logMatchesSearch(log, search),
  );
  const visibleActivityLogs = processLogs
    .filter(isNotableRestreamActivity)
    .filter((log) => logMatchesSearch(log, search));
  const searchSummaryText = statusLogSearchSummaryText(
    visibleActivityLogs.length,
    visibleProcessLogs.length,
    statusLogSearchQuery,
  );
  const toolchainRows = [
    row("Rust", data.toolchain?.rustc),
    row("Target", data.toolchain?.target),
    row("LLVM", data.toolchain?.llvm),
    row("GCC Runtime", data.toolchain?.gccRuntime),
  ].join("");
  const nativeRows = [
    row("FFmpeg", ffmpeg?.version),
    row("FFmpeg License", ffmpeg?.license),
    row("FFmpeg x86 Assembly", ffmpeg?.x86Assembly),
    versionRows("libsrt", srt?.version, srt?.buildVersion),
    row("libsrt License", srt?.license),
    row("SRT Bonding Available", srt?.bondingAvailable),
    versionRows("Mbed TLS", mbedtls?.version, mbedtls?.buildVersion),
    row("Mbed TLS License", mbedtls?.license),
    row("SQLite Version", sqlite?.version),
    row("SQLite License", sqlite?.license),
    row("x264 Version", data.nativeLibraries?.x264?.version),
    row("x264 License", data.nativeLibraries?.x264?.license),
    row("x264 Version Source", data.nativeLibraries?.x264?.versionSource),
    row("x265 Version", data.nativeLibraries?.x265?.version),
    row("x265 License", data.nativeLibraries?.x265?.license),
    row("x265 Version Source", data.nativeLibraries?.x265?.versionSource),
  ].join("");
  const sbomRows = [
    row("Endpoint", sbomEndpoint),
    row("Components", data.sbom?.componentCount),
    row("Rust Components", data.sbom?.rustComponentCount),
    row("Native Components", data.sbom?.nativeComponentCount),
    row("Native Component Names", formatList(data.sbom?.nativeComponents)),
    row("Licenses Included", data.sbom?.licensesIncluded),
  ].join("");

  container.innerHTML = [
    `<p id="status-route-summary" class="dashboard-muted text-sm" role="status" aria-live="polite">${escapeHtml(statusSummaryText(data, processLogs))}</p>`,
    `<div class="flex flex-wrap items-end gap-3">
            <label class="form-control w-full max-w-md">
                <span class="label-text text-base-content/70">Search process logs and activity</span>
                <input id="status-log-search" class="input input-sm input-bordered mt-1" type="search" value="${escapeHtml(statusLogSearchQuery)}" placeholder="level, target, event, message…" aria-label="Search process logs and activity" autocomplete="off" />
            </label>
            <button id="status-clear-search-btn" type="button" class="btn btn-sm btn-outline ${search ? "" : "hidden"}" aria-label="Clear status search">Clear search</button>
            <p id="status-log-search-results-summary" class="dashboard-muted pb-1 text-sm" role="status" aria-live="polite">${escapeHtml(searchSummaryText)}</p>
        </div>`,
    statusQuickNavHtml(),
    section(
      "status-build-section",
      "Application Build",
      [
        row("Version", data.restream?.version),
        row("Commit", data.restream?.commit),
        row("Native Build ID", data.restream?.nativeBuildId),
      ].join(""),
    ),
    section(
      "status-system-section",
      "System",
      [
        row("Platform", data.os?.platform),
        row("Architecture", data.os?.arch),
        row("Hostname", data.os?.hostname),
        row("Kernel", data.os?.kernelVersion),
        row("Uptime", formatUptime(data.os?.uptime)),
        row("Total Memory", formatBytes(data.os?.totalMem)),
        row("CPU", data.os?.cpu?.modelName),
        row("CPU Capacity", formatCpuCapacity(data.os?.cpu)),
        row("Virtualization", formatVirtualization(data.os?.cpu)),
        row("Acceleration Features", formatFlags(data.os?.cpu?.flags)),
      ].join(""),
    ),
    advancedSection(
      "status-toolchain-section",
      "Toolchain",
      `Rust ${valueOrDash(data.toolchain?.rustc)} · target ${valueOrDash(data.toolchain?.target)}`,
      toolchainRows,
    ),
    advancedSection(
      "status-native-section",
      "Native Libraries",
      `FFmpeg ${valueOrDash(ffmpeg?.version)} · libsrt ${valueOrDash(srt?.version)} · SQLite ${valueOrDash(sqlite?.version)}`,
      nativeRows,
    ),
    advancedSection(
      "status-sbom-section",
      "SBOM",
      `${valueOrDash(data.sbom?.componentCount)} components · ${valueOrDash(data.sbom?.nativeComponentCount)} native · licenses ${valueOrDash(data.sbom?.licensesIncluded)}`,
      sbomRows,
    ),
    renderRestreamActivity(processLogs, search, statusLogSearchQuery),
    renderProcessLog(processLogs, search, statusLogSearchQuery),
    statusExportActionsHtml(),
  ].join("");
  bindActions(data, sbomEndpoint);
}

function openStatusStream(): void {
  if (!statusStreamingEnabled() || !statusDataSnapshot) return;
  statusStream.sync({
    filters: {
      scope: "restream",
    },
    resumeAfterId: statusStreamLastEventId ?? latestStatusProcessLogId(),
    onLog: (data) => {
      rememberStatusProcessLogId(data);
      mergeStatusProcessLogs([data]);
      if (
        data.eventClass === "lifecycle" ||
        (!data.eventClass && Boolean(data.eventType))
      ) {
        handleDashboardRuntimeLifecycleLog(data);
      }
      renderStatusSnapshot();
    },
  });
}

export function setStatusStreamActive(active: boolean): void {
  statusStreamActive = active;
  if (!statusStreamingEnabled()) {
    closeStatusStream();
    return;
  }
  openStatusStream();
}

export function syncStatusStreamVisibility(): void {
  if (!statusStreamingEnabled()) {
    closeStatusStream();
    return;
  }
  openStatusStream();
}

export async function loadStatus(): Promise<void> {
  const container = document.getElementById("status-versions");
  if (!container) return;

  const [data, processHistory] = await Promise.all([
    getEngineStatus<StatusData>(),
    getRestreamHistory({ limit: STATUS_PROCESS_LOG_LIMIT, order: "desc" }),
  ]);
  if (!data) {
    container.innerHTML =
      '<p class="text-error text-sm">Failed to load status info.</p>';
    return;
  }
  statusDataSnapshot = data;
  statusProcessLogs = Array.isArray(processHistory?.logs)
    ? (processHistory?.logs as AppLogRow[])
    : [];
  syncProcessIndicatorFromLogs([...statusProcessLogs].reverse());
  const latestLog = latestStatusProcessLog(statusProcessLogs);
  if (
    latestLog &&
    (latestLog.eventClass === "lifecycle" ||
      (!latestLog.eventClass && Boolean(latestLog.eventType)))
  ) {
    handleDashboardRuntimeLifecycleLog(latestLog);
  }
  statusStreamLastEventId = latestStatusProcessLogId();
  renderStatusSnapshot();
  closeStatusStream();
  openStatusStream();
}
