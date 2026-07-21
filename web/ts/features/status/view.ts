import {
  getEngineSbomEndpoint,
  getEngineStatus,
  getRestreamHistory,
} from "../../core/api.js";
import { withBasePath } from "../../core/base-path.js";
import {
  copyText,
  escapeHtml,
  showCopiedNotification,
} from "../../core/utils.js";
import type { AppLogRow } from "../../types.js";
import type { StatusCheckpointModel } from "../status-view-model.js";
import { getStatusLogs, processStatusLogLine, setHasStatusDataSnapshot, setStatusUpdateCallback, syncStatusStreamVisibility } from "./log-stream.js";

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
    ffmpeg?: { version?: string; license?: string; x86Assembly?: boolean };
    srt?: { version?: string; buildVersion?: string; license?: string; bondingAvailable?: boolean };
    mbedtls?: { version?: string; buildVersion?: string; license?: string };
    sqlite?: { version?: string; sourceId?: string; license?: string };
    x264?: { version?: string; license?: string; versionSource?: string };
    x265?: { version?: string; license?: string; versionSource?: string };
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
  };
}

let statusData: StatusData | null = null;
let statusCheckpointCallback: ((model: StatusCheckpointModel | null) => void) | null = null;

export function configureStatusCheckpointPresentation(options: {
  onPresentation?: (model: StatusCheckpointModel | null) => void;
  onStateChange?: (model: StatusCheckpointModel | null) => void;
}): void {
  statusCheckpointCallback = options.onPresentation || options.onStateChange || null;
}

export async function loadStatus(): Promise<void> {
  const container =
    document.getElementById("status-versions") ||
    document.getElementById("status-mode-content");

  const [data, processHistory] = await Promise.all([
    getEngineStatus() as Promise<StatusData | null>,
    getRestreamHistory({ limit: 80, order: "desc" }),
  ]);

  if (processHistory && Array.isArray((processHistory as any).logs)) {
    ((processHistory as any).logs as any[]).slice().reverse().forEach((log) => processStatusLogLine(log));
  }

  statusData = data;
  setHasStatusDataSnapshot(Boolean(data));
  setStatusUpdateCallback(() => {
    const el =
      document.getElementById("status-versions") ||
      document.getElementById("status-mode-content");
    if (el && statusData) renderStatusContent(el, statusData);
  });
  syncStatusStreamVisibility();
  if (!container) return;
  if (!statusData) {
    container.innerHTML = '<div class="alert alert-error">Failed to load system status.</div>';
    return;
  }

  renderStatusContent(container, statusData);
}

function statusQuickNavHtml(): string {
  return `<nav class="dashboard-nav-strip" aria-label="Status sections">
      <label class="flex w-full max-w-xs flex-col gap-1 text-sm">
          <span class="text-base-content/60 text-xs font-medium uppercase tracking-[0.12em]">Jump to section</span>
          <select id="status-section-jump" class="select select-sm w-full" aria-label="Jump to status section">
              <option value="">Choose a status section…</option>
              <option value="status-build-section">Build</option>
              <option value="status-system-section">System</option>
              <option value="status-toolchain-section">Toolchain</option>
              <option value="status-native-section">Libraries</option>
              <option value="status-sbom-section">SBOM</option>
              <option value="status-activity-section">Activity</option>
              <option value="status-log-section">Logs</option>
          </select>
      </label>
    </nav>`;
}

function statusSummaryText(logs: AppLogRow[]): string {
  const count = logs.length;
  const notable = logs.filter((log) => {
    const eventType = String(log?.eventType || "").toLowerCase();
    const msg = String(log?.message || "");
    const level = String(log?.level || "").toUpperCase();
    return eventType.startsWith("restream.") || level === "WARN" || level === "ERROR" || /listening|shutdown|exited/i.test(msg);
  }).length;
  const activityLabel = notable === 1 ? "activity" : "activities";
  const logLabel = count === 1 ? "process log" : "process logs";
  return `${notable} ${activityLabel} · ${count} ${logLabel} visible`;
}

function renderStatusContent(container: HTMLElement, data: StatusData): void {
  const restream = data.restream || {};
  const toolchain = data.toolchain || {};
  const os = data.os || {};
  const logs = getStatusLogs();

  container.innerHTML = `
    <p id="status-route-summary" class="dashboard-muted text-sm" role="status" aria-live="polite">${escapeHtml(statusSummaryText(logs))}</p>
    <div class="dashboard-section space-y-6 p-6">
      <div class="flex items-center justify-between">
        <div>
          <h2 class="text-xl font-bold">System Status & Build Diagnostics</h2>
          <p class="text-sm opacity-70">Engine runtime, compiled dependencies, and platform topology</p>
        </div>
        <div class="flex items-center gap-2">
          <button type="button" class="btn btn-sm btn-outline js-copy-diagnostics">Copy Diagnostics</button>
          <a href="${withBasePath(getEngineSbomEndpoint(null))}" target="_blank" class="btn btn-sm btn-ghost">View Raw SBOM</a>
        </div>
      </div>

      <div class="flex flex-wrap items-end gap-3">
        <label class="form-control w-full max-w-md">
          <span class="label-text text-base-content/70">Search process logs and activity</span>
          <input id="status-log-search" class="input input-sm input-bordered mt-1" type="search" placeholder="level, target, event, message…" aria-label="Search process logs and activity" autocomplete="off" />
        </label>
      </div>

      ${statusQuickNavHtml()}

      <div class="grid grid-cols-1 gap-4 md:grid-cols-3">
        <div id="status-build-section" class="border-base-content/10 bg-base-200/40 rounded-lg border p-4">
          <h3 class="text-xs font-bold uppercase tracking-wider opacity-70">Core Engine</h3>
          <div class="mt-2 space-y-1 text-sm font-mono">
            <div><span class="opacity-70">Version:</span> ${escapeHtml(restream.version || "0.1.0")}</div>
            <div><span class="opacity-70">Commit:</span> ${escapeHtml(restream.commit || "dev")}</div>
            <div><span class="opacity-70">Build ID:</span> ${escapeHtml(restream.nativeBuildId || "unknown")}</div>
          </div>
        </div>

        <div id="status-toolchain-section" class="border-base-content/10 bg-base-200/40 rounded-lg border p-4">
          <h3 class="text-xs font-bold uppercase tracking-wider opacity-70">Compiler Toolchain</h3>
          <div class="mt-2 space-y-1 text-sm font-mono">
            <div><span class="opacity-70">Rustc:</span> ${escapeHtml(toolchain.rustc || "unknown")}</div>
            <div><span class="opacity-70">Target:</span> ${escapeHtml(toolchain.target || "unknown")}</div>
            <div><span class="opacity-70">LLVM:</span> ${escapeHtml(toolchain.llvm || "N/A")}</div>
          </div>
        </div>

        <div id="status-system-section" class="border-base-content/10 bg-base-200/40 rounded-lg border p-4">
          <h3 class="text-xs font-bold uppercase tracking-wider opacity-70">Host Environment</h3>
          <div class="mt-2 space-y-1 text-sm font-mono">
            <div><span class="opacity-70">Platform:</span> ${escapeHtml(os.platform || "linux")} (${escapeHtml(os.arch || "x86_64")})</div>
            <div><span class="opacity-70">Hostname:</span> ${escapeHtml(os.hostname || "localhost")}</div>
            <div><span class="opacity-70">Kernel:</span> ${escapeHtml(os.kernelVersion || "unknown")}</div>
          </div>
        </div>
      </div>

      <section id="status-native-section" class="space-y-2">
        <h3 class="text-sm font-bold">Native Libraries</h3>
      </section>

      <section id="status-sbom-section" class="space-y-2">
        <h3 class="text-sm font-bold">SBOM</h3>
      </section>

      <section id="status-activity-section" class="space-y-2">
        <h3 class="text-sm font-bold">Activity</h3>
      </section>

      <section id="status-log-section" class="space-y-2">
        <h3 class="text-sm font-bold">Process Log</h3>
        <div>${getStatusLogs().map((log) => `<div>${escapeHtml(log.message || "")}</div>`).join("")}</div>
      </section>
    </div>`;

  container.querySelector(".js-copy-diagnostics")?.addEventListener("click", () => {
    void copyText(JSON.stringify(data, null, 2)).then(() => {
      showCopiedNotification();
    });
  });
}
