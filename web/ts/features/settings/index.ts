import { getConfig } from "../../core/api.js";
import { state } from "../../core/state.js";
import { escapeHtml } from "../../core/utils.js";
import type { SettingsCheckpointModel } from "../settings-view-model.js";

import {
  addTranscodeProfile,
  loadTranscodeProfiles,
  saveTranscodeProfiles,
} from "./profiles.js";

import {
  populateBackendPolicySettings,
  populateRecordingSettings,
  populateSrtIngestSettings,
  saveBackendPolicy,
  saveIngestHost,
  saveRecordingSettings,
  saveServerName,
  saveSrtIngest,
} from "./config-sections.js";

import {
  dismissDashboardPasswordPrompt,
  logoutUser,
  populateIngestSecuritySettings,
  refreshRateLimitState,
  resetRateLimitStateFromUi,
  saveDashboardPassword,
  saveIngestSecurity,
  setSettingsRateLimitStateChangeHandler,
  settingsRateLimitPresentation,
  syncDashboardPasswordPrompt,
} from "./security.js";
const SETTINGS_SECTION_COUNT = 5;

let settingsCheckpointCallback:
  | ((model: SettingsCheckpointModel | null) => void)
  | null = null;

interface SettingsDisclosureConfig {
  readonly ariaLabel: string;
  readonly id: string;
  readonly summary: string;
  readonly title: string;
}

type RateLimitResetScope = "all" | "ip" | "username";

const SETTINGS_DISCLOSURES: readonly SettingsDisclosureConfig[] = [
  {
    id: "recording-settings-section",
    title: "Recording",
    summary: "Retention policy for completed MPEG-TS to MP4 conversions.",
    ariaLabel: "Recording settings",
  },
  {
    id: "dashboard-password-section",
    title: "Dashboard Password",
    summary: "Change the dashboard login password.",
    ariaLabel: "Dashboard password settings",
  },
  {
    id: "ingest-security-section",
    title: "Ingest Security",
    summary: "Failure thresholds, ban window, and tracked IP limits.",
    ariaLabel: "Ingest security settings",
  },
  {
    id: "auth-attempts-section",
    title: "Authentication Attempts",
    summary: "Recent login and publish failures with optional reset actions.",
    ariaLabel: "Authentication attempt settings",
  },
  {
    id: "srt-settings-section",
    title: "Global SRT Ingest",
    summary: "Default encryption policy for SRT publishers.",
    ariaLabel: "Global SRT ingest settings",
  },
  {
    id: "backend-policy-section",
    title: "Transcoding Backend",
    summary: "Backend selection for newly started transcoding stages.",
    ariaLabel: "Transcoding backend settings",
  },
  {
    id: "transcode-profiles-section",
    title: "Transcode Profiles",
    summary: "Encoder presets used by HEVC/H.264 and resolution workflows.",
    ariaLabel: "Transcode profile settings",
  },
];

function needsFullSettingsConfig(): boolean {
  return (
    (state.config as any)?.ingestSecurity === undefined ||
    (state.config as any)?.recordingSettings === undefined ||
    (state.config as any)?.srtIngest === undefined ||
    (state.config as any)?.backendPolicy === undefined
  );
}

function effectiveServerName(): string {
  return state.config?.serverName?.trim() || "Restream";
}

async function ensureFullSettingsConfig(): Promise<void> {
  if (!needsFullSettingsConfig()) return;
  const fullConfig = await getConfig();
  if (!fullConfig) return;
  state.config = {
    ...state.config,
    ...fullConfig,
  };
}

export async function loadSettings({
  embedded = false,
}: { embedded?: boolean } = {}): Promise<void> {
  await ensureFullSettingsConfig();
  const nameInput = document.getElementById(
    "settings-server-name",
  ) as HTMLInputElement | null;
  if (nameInput) nameInput.value = effectiveServerName();
  const hostInput = document.getElementById(
    "settings-ingest-host",
  ) as HTMLInputElement | null;
  if (hostInput) hostInput.value = state.config?.ingestHost || "";
  populateIngestSecuritySettings();
  populateRecordingSettings();
  populateSrtIngestSettings();
  populateBackendPolicySettings();
  syncDashboardPasswordPrompt();
  void refreshRateLimitState();
  loadTranscodeProfiles();
  updateSettingsSummary();
}

export function configureSettingsCheckpointPresentation(options: {
  onPresentation?: (model: SettingsCheckpointModel | null) => void;
  onStateChange?: (model: SettingsCheckpointModel | null) => void;
}): void {
  settingsCheckpointCallback = options.onPresentation || options.onStateChange || null;
  if (settingsCheckpointCallback) publishSettingsCheckpoint();
}

export function registerSettingsGlobals(): void {
  // Global event handlers bound on demand
}

function pluralize(
  count: number,
  singular: string,
  plural = `${singular}s`,
): string {
  return `${count} ${count === 1 ? singular : plural}`;
}

function countConfiguredProfiles(): number {
  const list = document.getElementById("transcode-profiles-list");
  if (!list) {
    return Object.keys(state.config?.transcodeProfiles || {}).length || 1;
  }
  const rendered = list.querySelectorAll("[data-profile-name]").length;
  return rendered || Object.keys(state.config?.transcodeProfiles || {}).length || 1;
}

function settingsSummaryText(): string {
  const rateLimit = settingsRateLimitPresentation();
  const serverName = effectiveServerName();
  return `${serverName} settings · ${pluralize(SETTINGS_SECTION_COUNT, "section")} · ${pluralize(countConfiguredProfiles(), "profile")} · ${pluralize(rateLimit.totalCount, "auth attempt")}`;
}

function buildSettingsCheckpointModel(): SettingsCheckpointModel {
  const rateLimit = settingsRateLimitPresentation();
  return {
    authLabel: pluralize(rateLimit.totalCount, "auth attempt"),
    canOpenStatus: true,
    focusLabel: rateLimit.query
      ? `${rateLimit.filteredCount} authentication attempt${rateLimit.filteredCount === 1 ? "" : "s"} match "${rateLimit.query}". Clear search before changing global rate-limit settings.`
      : rateLimit.bannedCount > 0
        ? `${rateLimit.bannedCount} authentication attempt${rateLimit.bannedCount === 1 ? " is" : "s are"} currently banned; review the table before resetting global limits.`
        : "Configuration sections stay grouped by operational concern; use the section rail before editing dense forms.",
    metrics: [
      { label: "Server", value: effectiveServerName() },
      {
        label: "Security",
        value: rateLimit.bannedCount
          ? pluralize(rateLimit.bannedCount, "banned attempt")
          : "No bans",
      },
      { label: "Ingest host", value: state.config?.ingestHost || "default host" },
    ],
    nextStep:
      rateLimit.bannedCount > 0
        ? "Review or reset the banned attempts, then open Status to confirm the service is healthy."
        : "Edit the needed section, save, then open Status to confirm runtime health.",
    profileLabel: pluralize(countConfiguredProfiles(), "profile"),
    searchLabel: rateLimit.searchLabel,
    sectionLabel: pluralize(SETTINGS_SECTION_COUNT, "section"),
    securityLabel: rateLimit.bannedCount
      ? pluralize(rateLimit.bannedCount, "banned attempt")
      : "No bans",
    statusLabel: rateLimit.query
      ? "Filtered"
      : rateLimit.bannedCount
        ? "Review"
        : "Loaded",
    statusTone:
      rateLimit.query || rateLimit.bannedCount ? "warning" : "success",
    summary: settingsSummaryText(),
    title: "Settings",
  };
}

function publishSettingsCheckpoint(): void {
  settingsCheckpointCallback?.(buildSettingsCheckpointModel());
}

function updateSettingsSummary(): void {
  const summary = document.getElementById("settings-route-summary");
  if (summary) summary.textContent = settingsSummaryText();
  publishSettingsCheckpoint();
}

function settingsNavHtml(id = ""): string {
  const selectId = id ? `${id}-section-jump` : "settings-section-jump";
  return `<nav${id ? ` id="${id}"` : ""} class="dashboard-nav-strip w-full" aria-label="Settings sections">
      <label class="flex w-full max-w-xs flex-col gap-1 text-sm">
          <span class="text-base-content/60 text-xs font-medium uppercase tracking-[0.12em]">Jump to section</span>
          <select id="${selectId}" class="select select-sm w-full" data-settings-section-jump aria-label="Jump to settings section">
              <option value="">Choose a settings section…</option>
              <option value="server-settings-section">Server</option>
              <option value="recording-settings-section">Recording</option>
              <option value="srt-settings-section">SRT</option>
              <option value="backend-policy-section">Backend</option>
              <option value="transcode-profiles-section">Profiles</option>
          </select>
      </label>
  </nav>`;
}

function bindSettingsSectionJump(container: HTMLElement): void {
  container
    .querySelectorAll<HTMLSelectElement>("[data-settings-section-jump]")
    .forEach((select) => {
      select.addEventListener("change", () => {
        const targetId = select.value;
        if (!targetId) return;
        const target = document.getElementById(targetId);
        if (!target) return;
        if (target instanceof HTMLDetailsElement) target.open = true;
        target.scrollIntoView({ block: "start" });
        history.replaceState(null, "", `#${targetId}`);
      });
    });
}

function mountSettingsV2Disclosures(container: HTMLElement): void {
  for (const disclosure of SETTINGS_DISCLOSURES) {
    const body = container.querySelector<HTMLElement>(`#${disclosure.id}`);
    if (!body || body.closest("[data-settings-v2-disclosure]")) continue;
    const wrapper = document.createElement("details");
    wrapper.id = disclosure.id;
    wrapper.className =
      "border-base-content/10 bg-base-100/60 rounded-lg border px-3 py-2";
    wrapper.dataset.settingsV2Disclosure = disclosure.id;
    wrapper.innerHTML = `<summary class="flex cursor-pointer list-none flex-wrap items-center justify-between gap-2" aria-label="${escapeHtml(disclosure.ariaLabel)}">
        <span>
          <h2 class="text-sm font-semibold">${escapeHtml(disclosure.title)}</h2>
          <span class="text-base-content/60 mt-1 block text-xs">${escapeHtml(disclosure.summary)}</span>
        </span>
        <span class="btn btn-xs btn-outline pointer-events-none">Show settings</span>
      </summary>`;
    body.removeAttribute("id");
    body.classList.add("mt-3");
    body.dataset.settingsV2DisclosureBody = disclosure.id;
    body.replaceWith(wrapper);
    wrapper.append(body);
  }
}

function settingsResetScope(value: string | undefined): RateLimitResetScope {
  switch (value) {
    case "ip":
    case "username":
      return value;
    default:
      return "all";
  }
}

function syncSettingsAccountActions(container: ParentNode = document): void {
  const toggle = container.querySelector<HTMLButtonElement>(
    "#settings-account-actions-toggle",
  );
  const logoutButton = container.querySelector<HTMLButtonElement>(
    "#settings-logout-btn",
  );
  const expanded = toggle?.getAttribute("aria-expanded") === "true";
  toggle?.classList.remove("hidden");
  logoutButton?.classList.toggle("hidden", !expanded);
  if (toggle) {
    toggle.textContent = expanded ? "Hide account actions" : "Show account actions";
  }
}

function bindSettingsPanelActions(container: HTMLElement): void {
  container
    .querySelector<HTMLButtonElement>("#settings-account-actions-toggle")
    ?.addEventListener("click", (event) => {
      const button = event.currentTarget as HTMLButtonElement;
      const expanded = button.getAttribute("aria-expanded") === "true";
      button.setAttribute("aria-expanded", expanded ? "false" : "true");
      syncSettingsAccountActions(container);
      button.focus();
    });
}

function renderSettingsRoute(
  container: HTMLElement,
  options: { readonly routeChrome?: boolean } = {},
): void {
  const serverNameValue = escapeHtml(effectiveServerName());
  const routeChrome = options.routeChrome ?? true;
  const routeHeader = routeChrome
    ? `<div class="flex flex-wrap items-end justify-between gap-3">
                <div>
                    <h1 class="dashboard-title">Settings</h1>
                    <p class="dashboard-subtitle">Server, security, and encoding configuration.</p>
                </div>
            </div>`
    : "";
  container.innerHTML = `
        <div class="dashboard-page-shell">
            ${routeHeader}
            ${settingsNavHtml("settings-admin-nav")}
            <p id="settings-route-summary" class="text-base-content/60 text-sm" role="status" aria-live="polite"></p>

            <section id="server-settings-section" class="dashboard-section space-y-5 p-5">
                <div>
                    <h2 class="dashboard-section-title">Server</h2>
                </div>

                <div class="space-y-4">
                    <div class="max-w-2xl space-y-2">
                        <label for="settings-server-name" class="text-sm font-medium">Server Name</label>
                        <div class="flex flex-wrap items-center gap-2">
                            <input type="text" id="settings-server-name" class="input input-sm min-w-0 flex-1" placeholder="Name" aria-label="Server name" value="${serverNameValue}" />
                            <button class="btn btn-accent btn-sm" data-settings-action="save-server-name" aria-label="Save server name">Save</button>
                            <span id="server-name-saved" class="text-success hidden text-sm">Saved</span>
                        </div>
                    </div>

                    <div class="max-w-2xl space-y-2">
                        <label for="settings-ingest-host" class="text-sm font-medium">Ingest Host</label>
                        <div class="flex flex-wrap items-center gap-2">
                            <input
                                type="text"
                                id="settings-ingest-host"
                                class="input input-sm min-w-0 flex-1"
                                placeholder="e.g. 192.168.1.10 (blank = localhost)"
                                aria-label="Ingest host" />
                            <button class="btn btn-accent btn-sm" data-settings-action="save-ingest-host" aria-label="Save ingest host">Save</button>
                            <span id="ingest-host-saved" class="text-success hidden text-sm">Saved</span>
                        </div>
                    </div>
                </div>

                <div class="divider my-0"></div>

                <div id="dashboard-password-prompt" class="alert alert-warning hidden items-start gap-3">
                    <div class="min-w-0">
                        <div class="font-semibold">Initial dashboard password is still active</div>
                        <div class="text-sm">Set a different password below, or dismiss this reminder.</div>
                    </div>
                    <button class="btn btn-sm" data-settings-action="dismiss-dashboard-password-prompt">Skip</button>
                </div>

                <div id="dashboard-password-section" class="space-y-2">
                    <div class="text-sm font-medium">Dashboard Password</div>
                    <div class="flex flex-wrap items-end gap-3">
                        <fieldset class="fieldset">
                            <legend class="fieldset-legend">Current Password</legend>
                            <input type="password" id="current-password-input" class="input input-sm w-44" autocomplete="current-password" aria-label="Current password" />
                        </fieldset>
                        <fieldset class="fieldset">
                            <legend class="fieldset-legend">New Password</legend>
                            <input type="password" id="new-password-input" class="input input-sm w-44" autocomplete="new-password" minlength="12" aria-label="New password" />
                        </fieldset>
                        <fieldset class="fieldset">
                            <legend class="fieldset-legend">Confirm Password</legend>
                            <input type="password" id="confirm-password-input" class="input input-sm w-44" autocomplete="new-password" minlength="12" aria-label="Confirm password" />
                        </fieldset>
                        <fieldset class="fieldset">
                            <legend class="fieldset-legend invisible">_</legend>
                            <div class="flex items-center gap-3">
                                <button class="btn btn-accent btn-sm" data-settings-action="save-dashboard-password" aria-label="Save dashboard password">Save</button>
                                <span id="dashboard-password-saved" class="text-success hidden text-sm">Saved</span>
                            </div>
                        </fieldset>
                    </div>
                </div>

                <div class="divider my-0"></div>

                <div id="ingest-security-section" class="space-y-2">
                    <div class="text-sm font-medium">Ingest Security</div>
                    <div class="flex flex-wrap items-end gap-3">
                        <fieldset class="fieldset">
                            <legend class="fieldset-legend">Failure Limit</legend>
                            <input type="number" id="ingest-security-failure-limit" class="input input-sm w-28" min="1" step="1" aria-label="Ingest security failure limit" />
                        </fieldset>
                        <fieldset class="fieldset">
                            <legend class="fieldset-legend">Failure Window (ms)</legend>
                            <input type="number" id="ingest-security-failure-window-ms" class="input input-sm w-36" min="1" step="1" aria-label="Ingest security failure window in milliseconds" />
                        </fieldset>
                        <fieldset class="fieldset">
                            <legend class="fieldset-legend">Ban Duration (ms)</legend>
                            <input type="number" id="ingest-security-ban-ms" class="input input-sm w-36" min="1" step="1" aria-label="Ingest security ban duration in milliseconds" />
                        </fieldset>
                        <fieldset class="fieldset">
                            <legend class="fieldset-legend">Tracked IP Limit</legend>
                            <input type="number" id="ingest-security-tracked-ip-limit" class="input input-sm w-32" min="1" step="1" aria-label="Ingest security tracked IP limit" />
                        </fieldset>
                        <fieldset class="fieldset">
                            <legend class="fieldset-legend invisible">_</legend>
                            <div class="flex items-center gap-3">
                                <button class="btn btn-accent btn-sm" data-settings-action="save-ingest-security" aria-label="Save ingest security settings">Save</button>
                                <span id="ingest-security-saved" class="text-success hidden text-sm">Saved</span>
                            </div>
                        </fieldset>
                    </div>
                </div>

                <div id="auth-attempts-section" class="space-y-2">
                    <div class="flex flex-wrap items-center justify-between gap-2">
                        <div class="text-sm font-medium">Authentication Attempts</div>
                        <div class="flex items-center gap-2">
                            <button class="btn btn-ghost btn-sm" data-settings-action="refresh-rate-limits" aria-label="Refresh authentication attempts">Refresh</button>
                            <button id="auth-reset-actions-toggle" type="button" class="btn btn-outline btn-sm hidden" aria-label="Show authentication reset actions" aria-expanded="false">Show reset actions</button>
                            <button id="auth-reset-all-btn" class="btn btn-outline btn-sm" data-settings-action="reset-rate-limits" aria-label="Reset all authentication attempts">Reset All</button>
                        </div>
                    </div>
                    <div class="flex flex-wrap items-end gap-3">
                        <label class="form-control w-full max-w-md">
                            <span class="label-text text-base-content/70">Search authentication attempts</span>
                            <input id="auth-attempts-search" class="input input-sm input-bordered mt-1" type="search" value="" placeholder="scope, IP, banned, tracking…" aria-label="Search authentication attempts" autocomplete="off" />
                        </label>
                        <button id="auth-attempts-clear-search-btn" type="button" class="btn btn-sm btn-outline hidden" aria-label="Clear authentication attempt search">Clear search</button>
                        <button id="auth-attempts-toggle" type="button" class="btn btn-sm btn-outline hidden" aria-label="Show all authentication attempts" aria-expanded="false">Show all</button>
                        <p id="auth-attempts-search-summary" class="text-base-content/60 pb-1 text-sm" role="status" aria-live="polite">0 auth attempts visible</p>
                    </div>
                    <div class="overflow-x-auto rounded-lg border border-base-content/10" role="region" aria-label="Authentication attempts" tabindex="0">
                        <table class="table table-sm">
                            <thead>
                                <tr>
                                    <th>Scope</th>
                                    <th>IP</th>
                                    <th>Failures</th>
                                    <th>Status</th>
                                    <th id="auth-attempts-reset-heading" class="text-right">Reset</th>
                                </tr>
                            </thead>
                            <tbody id="rate-limit-attempts-body">
                                <tr><td colspan="5" class="text-base-content/60">No attempts</td></tr>
                            </tbody>
                        </table>
                    </div>
                </div>

                <div class="divider my-0"></div>

                <div id="recording-settings-section" class="space-y-2">
                    <div class="text-sm font-medium">Recording</div>
                    <p class="text-base-content/60 text-sm">Choose whether completed recordings keep the original MPEG-TS source after a successful MP4 conversion.</p>
                    <div class="flex flex-wrap items-end gap-3">
                        <label class="border-base-content/10 bg-base-100 flex min-w-[22rem] flex-1 items-start gap-3 rounded-lg border px-3 py-3">
                            <input type="checkbox" id="recording-retain-source-ts" class="checkbox checkbox-sm mt-0.5" />
                            <div class="space-y-1">
                                <div class="text-sm font-medium">Keep original <code>.ts</code> after successful conversion</div>
                                <div class="text-base-content/60 text-sm">Unchecked by default to save storage. Failed conversions always keep the source file.</div>
                            </div>
                        </label>
                        <fieldset class="fieldset">
                            <legend class="fieldset-legend invisible">_</legend>
                            <div class="flex items-center gap-3">
                                <button class="btn btn-accent btn-sm" data-settings-action="save-recording-settings" aria-label="Save recording settings">Save</button>
                                <span id="recording-settings-saved" class="text-success hidden text-sm">Saved</span>
                            </div>
                        </fieldset>
                    </div>
                </div>

                <div class="divider my-0"></div>

                <div id="srt-settings-section" class="space-y-2">
                    <div class="text-sm font-medium">Global SRT Ingest</div>
                    <p class="text-base-content/60 text-sm">Pipelines can inherit the global SRT policy, force plaintext, or use their own passphrase.</p>
                    <div class="flex flex-wrap items-end gap-3">
                        <fieldset class="fieldset">
                            <legend class="fieldset-legend">Mode</legend>
                            <select id="srt-ingest-mode-input" class="select select-sm w-40" aria-label="Global SRT ingest mode">
                                <option value="plaintext">Plaintext</option>
                                <option value="encrypted">Encrypted</option>
                            </select>
                        </fieldset>
                        <fieldset class="fieldset">
                            <legend class="fieldset-legend">Passphrase</legend>
                            <input type="password" id="srt-ingest-passphrase-input" class="input input-sm w-52" placeholder="10-79 bytes" />
                        </fieldset>
                        <fieldset class="fieldset">
                            <legend class="fieldset-legend">Key Length</legend>
                            <select id="srt-ingest-pbkeylen-input" class="select select-sm w-28" aria-label="Global SRT ingest key length">
                                <option value="16">AES-128</option>
                                <option value="24">AES-192</option>
                                <option value="32">AES-256</option>
                            </select>
                        </fieldset>
                        <fieldset class="fieldset">
                            <legend class="fieldset-legend">Latency (ms)</legend>
                            <input type="number" id="srt-ingest-latency-ms-input" class="input input-sm w-28" min="20" max="8000" step="1" aria-label="Global SRT ingest latency in milliseconds" />
                        </fieldset>
                        <fieldset class="fieldset">
                            <legend class="fieldset-legend invisible">_</legend>
                            <div class="flex items-center gap-3">
                                <button class="btn btn-accent btn-sm" data-settings-action="save-srt-ingest" aria-label="Save global SRT ingest settings">Save</button>
                                <span id="srt-ingest-saved" class="text-success hidden text-sm">Saved</span>
                            </div>
                        </fieldset>
                    </div>
                    <p class="text-base-content/60 text-xs">This caller's own proposed minimum TSBPD delay (SRTO_RCVLATENCY) for every ingest connection that doesn't set a per-pipeline override. 20&ndash;8000ms.</p>
                </div>

                <div class="divider my-0"></div>

                <div id="backend-policy-section" class="space-y-2">
                    <div class="text-sm font-medium">Transcoding Backend</div>
                    <p class="text-base-content/60 text-sm">Policy for newly started or reconciled stages.</p>
                    <div class="grid gap-2 sm:grid-cols-2">
                        <label class="border-base-content/10 bg-base-100 flex items-start gap-3 rounded-lg border px-3 py-3">
                            <input type="checkbox" id="backend-policy-internal-video-presets" class="checkbox checkbox-sm mt-0.5" />
                            <span class="text-sm">Use internal backend for video presets</span>
                        </label>
                        <label class="border-base-content/10 bg-base-100 flex items-start gap-3 rounded-lg border px-3 py-3">
                            <input type="checkbox" id="backend-policy-internal-hevc-to-h264" class="checkbox checkbox-sm mt-0.5" />
                            <span class="text-sm">Use internal backend for HEVC to H.264</span>
                        </label>
                        <label class="border-base-content/10 bg-base-100 flex items-start gap-3 rounded-lg border px-3 py-3">
                            <input type="checkbox" id="backend-policy-internal-hls-preview" class="checkbox checkbox-sm mt-0.5" />
                            <span class="text-sm">Use internal backend for HLS preview</span>
                        </label>
                        <label class="border-base-content/10 bg-base-100 flex items-start gap-3 rounded-lg border px-3 py-3">
                            <input type="checkbox" id="backend-policy-internal-complex-audio" class="checkbox checkbox-sm mt-0.5" />
                            <span class="text-sm">Use internal backend for complex audio</span>
                        </label>
                    </div>
                    <div class="flex items-center gap-3">
                        <button class="btn btn-accent btn-sm" data-settings-action="save-backend-policy" aria-label="Save transcoding backend policy">Save</button>
                        <span id="backend-policy-saved" class="text-success hidden text-sm">Saved</span>
                    </div>
                </div>

                <div class="divider my-0"></div>

                <div id="transcode-profiles-section" class="space-y-3">
                    <div class="flex flex-wrap items-baseline gap-3">
                        <span class="shrink-0 text-sm font-medium">Transcode Profiles</span>
                        <span class="text-sm opacity-70">Encoder settings per profile name.</span>
                    </div>
                    <div id="transcode-profiles-list" class="space-y-3"></div>
                    <div class="flex items-center gap-3">
                        <button class="btn btn-accent btn-sm" data-settings-action="save-transcode-profiles">Save Profiles</button>
                        <button class="btn btn-ghost btn-sm" data-settings-action="add-transcode-profile">+ Add Profile</button>
                        <span id="transcode-profiles-saved" class="text-success hidden text-sm">Saved</span>
                    </div>
                </div>

                <div class="flex flex-wrap justify-end gap-2">
                    <button id="settings-account-actions-toggle" type="button" class="btn btn-outline btn-sm hidden" aria-expanded="false">Show account actions</button>
                    <button id="settings-logout-btn" class="btn btn-error btn-outline btn-sm" data-settings-action="logout">Logout</button>
                </div>
            </section>
        </div>`;

  container.dataset.settingsRouteBody = "v2";
  mountSettingsV2Disclosures(container);
  syncSettingsAccountActions(container);
  bindSettingsSectionJump(container);
  bindSettingsPanelActions(container);
  setSettingsRateLimitStateChangeHandler(updateSettingsSummary);

  container.onclick = (event) => {
    const button = (event.target as Element | null)?.closest(
      "[data-settings-action]",
    ) as HTMLElement | null;
    if (!button || !container.contains(button)) return;
    const action = button.dataset.settingsAction;
    switch (action) {
      case "save-server-name":
        void saveServerName();
        break;
      case "save-ingest-host":
        void saveIngestHost();
        break;
      case "dismiss-dashboard-password-prompt":
        void dismissDashboardPasswordPrompt();
        break;
      case "save-dashboard-password":
        void saveDashboardPassword();
        break;
      case "save-ingest-security":
        void saveIngestSecurity();
        break;
      case "refresh-rate-limits":
        void refreshRateLimitState();
        break;
      case "reset-rate-limits":
        void resetRateLimitStateFromUi(
          settingsResetScope(button.dataset.scope),
          button.dataset.ip,
        );
        break;
      case "save-recording-settings":
        void saveRecordingSettings();
        break;
      case "save-srt-ingest":
        void saveSrtIngest();
        break;
      case "save-backend-policy":
        void saveBackendPolicy();
        break;
      case "save-transcode-profiles":
        void saveTranscodeProfiles();
        break;
      case "add-transcode-profile":
        addTranscodeProfile();
        break;
      case "logout":
        void logoutUser();
        break;
    }
  };

  void loadSettings({ embedded: true });
  updateSettingsSummary();
}

export function renderDashboardV2SettingsBody(container: HTMLElement): void {
  renderSettingsRoute(container, { routeChrome: false });
}

export {
  saveServerName,
  saveIngestHost,
  saveDashboardPassword,
  dismissDashboardPasswordPrompt,
  logoutUser,
  saveIngestSecurity,
  refreshRateLimitState,
  resetRateLimitStateFromUi,
  saveRecordingSettings,
  saveSrtIngest,
  saveBackendPolicy,
  loadTranscodeProfiles,
  addTranscodeProfile,
  saveTranscodeProfiles,
};
