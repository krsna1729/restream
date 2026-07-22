import { escapeHtml } from "../../core/utils.js";

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

export function statusExportActionsHtml(options: {
  expanded: boolean;
  v2Active: boolean;
}): string {
  const actions = `
            <div class="flex flex-wrap gap-2">
                <button type="button" class="btn btn-sm btn-outline" id="download-status-btn">Download status report</button>
                <button type="button" class="btn btn-sm btn-outline" id="copy-status-btn">Copy status report</button>
                <button type="button" class="btn btn-sm btn-outline" id="download-sbom-btn">Download SBOM file</button>
                <button type="button" class="btn btn-sm btn-outline" id="copy-sbom-btn">Copy SBOM file</button>
            </div>`;
  if (!options.v2Active) return actions;
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
