import { escapeHtml, formatByteAmount } from "../../core/utils.js";
import type { HostSettingRow } from "../../types.js";

export interface StatusCpu {
  modelName?: string | null;
  logicalCpus?: number;
  physicalCores?: number | null;
  threadsPerCore?: number | null;
  virtualization?: string | null;
  hypervisorDetected?: boolean;
  hypervisorVendor?: string | null;
  flags?: string[];
}

export function valueOrDash(value: unknown): string {
  if (value === null || value === undefined || value === "") return "--";
  if (typeof value === "boolean") return value ? "yes" : "no";
  return String(value);
}

export function row(label: string, value: unknown): string {
  return `<tr>
        <td class="text-base-content/65 py-1.5 pr-4 align-top font-medium whitespace-nowrap">${escapeHtml(label)}</td>
        <td class="py-1.5 align-top font-mono text-sm break-all">${escapeHtml(valueOrDash(value))}</td>
    </tr>`;
}

function formatThreadsPerCore(value: unknown): string {
  const n = Number(value);
  if (!Number.isFinite(n) || n <= 0) return "--";
  return Number.isInteger(n) ? n.toFixed(0) : n.toFixed(1);
}

export function formatCpuCapacity(cpu: StatusCpu | undefined): string {
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

export function formatFlags(value: unknown): string {
  if (!Array.isArray(value) || value.length === 0) return "--";
  return value.map((flag) => String(flag)).join(", ");
}

export function formatList(value: unknown): string {
  if (!Array.isArray(value) || value.length === 0) return "--";
  return value.map((item) => String(item)).join(", ");
}

export function formatVirtualization(cpu: StatusCpu | undefined): string {
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

export function versionRows(
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

export function formatUptime(value: unknown): string {
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

function formatHostCapacityValue(value: unknown, unit: unknown): string {
  if (typeof value === "string") return value;
  if (typeof value !== "number" || !Number.isFinite(value)) return "--";
  return unit === "bytes" ? formatByteAmount(value) : value.toLocaleString();
}

function hostCapacityTone(status: unknown): string {
  if (status === "ok") return "badge-success";
  if (status === "warning") return "badge-warning";
  return "badge-ghost";
}

function hostCapacityRows(settings: readonly HostSettingRow[] | undefined): string {
  if (!settings?.length) {
    return `<tr><td colspan="5" class="dashboard-muted py-2">Host-capacity data is unavailable.</td></tr>`;
  }
  return settings
    .map(
      (setting) => `<tr>
        <td class="py-2 pr-3 align-top"><div class="font-medium">${escapeHtml(setting.label || setting.key)}</div><div class="text-base-content/55 font-mono text-xs">${escapeHtml(setting.key)}</div></td>
        <td class="py-2 pr-3 align-top font-mono text-sm">${escapeHtml(formatHostCapacityValue(setting.current, setting.unit))}</td>
        <td class="py-2 pr-3 align-top font-mono text-sm">${escapeHtml(formatHostCapacityValue(setting.required, setting.unit))}</td>
        <td class="py-2 pr-3 align-top"><span class="badge badge-sm ${hostCapacityTone(setting.status)}">${escapeHtml(setting.status || "unknown")}</span></td>
        <td class="text-base-content/70 py-2 align-top text-sm">${escapeHtml(setting.detail || "--")}</td>
      </tr>`,
    )
    .join("");
}

export function hostCapacitySection(
  settings: readonly HostSettingRow[] | undefined,
  expanded: boolean,
): string {
  const warnings = settings?.filter((setting) => setting.status === "warning").length ?? 0;
  const summary = settings?.length
    ? `${settings.length} host settings · ${warnings ? `${warnings} need attention` : "all reported limits satisfied"}`
    : "Loading host runtime limits and kernel ceilings";
  const rows = `<div class="overflow-x-auto" role="region" aria-label="Host capacity settings" tabindex="0">
      <table class="w-full min-w-[52rem] table-fixed text-sm">
        <colgroup><col class="w-56" /><col class="w-32" /><col class="w-32" /><col class="w-24" /><col /></colgroup>
        <thead><tr class="text-base-content/60 text-left text-xs"><th class="pb-2 pr-3">Setting</th><th class="pb-2 pr-3">Current</th><th class="pb-2 pr-3">Required</th><th class="pb-2 pr-3">Status</th><th class="pb-2">Why</th></tr></thead>
        <tbody>${hostCapacityRows(settings)}</tbody>
      </table>
    </div>`;
  return advancedSection(
    "status-host-capacity-section",
    "Host Capacity",
    summary,
    rows,
    expanded,
  );
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

export function advancedSection(
  id: string,
  title: string,
  summary: string,
  rows: string,
  expanded: boolean,
): string {
  if (expanded) {
    const hideLabel = advancedSectionActionLabel("Hide", title);
    return `${section(id, title, rows)}
      <button type="button" class="btn btn-xs btn-outline mt-2" data-status-advanced-section="${escapeHtml(id)}" aria-label="${escapeHtml(hideLabel)}" aria-expanded="true">Hide ${escapeHtml(title)} details</button>`;
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

const STATUS_SECTION_NAV = [
  {
    id: "status-build-section",
    label: "Build",
  },
  {
    id: "status-system-section",
    label: "System",
  },
  {
    id: "status-host-capacity-section",
    label: "Host capacity",
  },
  {
    id: "status-toolchain-section",
    label: "Toolchain",
  },
  {
    id: "status-native-section",
    label: "Libraries",
  },
  {
    id: "status-sbom-section",
    label: "SBOM",
  },
  {
    id: "status-activity-section",
    label: "Activity",
  },
  {
    id: "status-log-section",
    label: "Logs",
  },
] as const;

export function statusExportActionsHtml(options: { expanded: boolean }): string {
  const actions = `
            <div class="flex flex-wrap gap-2">
                <button type="button" class="btn btn-sm btn-outline" id="download-status-btn">Download status report</button>
                <button type="button" class="btn btn-sm btn-outline" id="copy-status-btn">Copy status report</button>
                <button type="button" class="btn btn-sm btn-outline" id="download-sbom-btn">Download SBOM file</button>
                <button type="button" class="btn btn-sm btn-outline" id="copy-sbom-btn">Copy SBOM file</button>
            </div>`;
  return `
        <section class="border-base-content/10 bg-base-100 rounded-2xl border p-4 shadow-sm" aria-label="Status export actions">
            <div class="flex flex-wrap items-start justify-between gap-3">
                <div>
                    <div class="text-sm font-semibold">Export actions</div>
                    <p class="dashboard-muted mt-1 text-sm">Download or copy runtime/SBOM evidence only when preparing an audit bundle.</p>
                </div>
                <button type="button" class="btn btn-sm btn-outline" id="status-export-actions-toggle" aria-label="${options.expanded ? "Hide status export actions" : "Show status export actions"}" aria-expanded="${options.expanded ? "true" : "false"}">
                    ${options.expanded ? "Hide export actions" : "Show export actions"}
                </button>
            </div>
            ${options.expanded ? `<div class="mt-3">${actions}</div>` : ""}
        </section>`;
}

export function statusQuickNavHtml(): string {
  return `<nav class="dashboard-nav-strip" aria-label="Status sections">
      <label class="flex w-full max-w-xs flex-col gap-1 text-sm">
          <span class="text-base-content/60 text-xs font-medium uppercase tracking-[0.12em]">Jump to section</span>
          <select id="status-section-jump" class="select select-sm w-full" aria-label="Jump to status section">
              <option value="">Choose a status section...</option>
              ${STATUS_SECTION_NAV.map(
                (item) =>
                  `<option value="${escapeHtml(item.id)}">${escapeHtml(item.label)}</option>`,
              ).join("")}
          </select>
      </label>
    </nav>`;
}
