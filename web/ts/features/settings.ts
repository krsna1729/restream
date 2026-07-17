import {
  getConfig,
  patchConfig,
  logout,
  changePassword,
  dismissPasswordChangePrompt,
  getRateLimitState,
  resetRateLimitState,
  type RateLimitAttempt,
  type TranscodeProfile,
  type TranscodeProfiles,
} from "../core/api.js";
import type {
  BackendPolicy,
  RecordingSettings,
  SrtGlobalIngestConfig,
} from "../types.js";
import type { SettingsCheckpointModel } from "./settings-view-model.js";
import { showErrorAlert } from "../core/utils.js";
import { state } from "../core/state.js";
import { withBasePath } from "../core/base-path.js";

const SETTINGS_SECTION_COUNT = 5;
const AUTH_ATTEMPT_VISIBLE_LIMIT = 8;
let lastRateLimitAttemptCount = 0;
let lastRateLimitAttempts: RateLimitAttempt[] = [];
let rateLimitSearchQuery = "";
let authAttemptsExpanded = false;
let authResetActionsExpanded = false;
let settingsAccountActionsExpanded = false;
let settingsCheckpointCallback:
  | ((model: SettingsCheckpointModel | null) => void)
  | null = null;

interface SettingsDisclosureConfig {
  id: string;
  title: string;
  summary: string;
}

// ── Load ──────────────────────────────────────────────

function needsFullSettingsConfig(): boolean {
  return (
    state.config?.ingestSecurity === undefined ||
    state.config?.recordingSettings === undefined ||
    state.config?.srtIngest === undefined ||
    state.config?.backendPolicy === undefined
  );
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
  if (!embedded) applySettingsChrome();
  const nameInput = document.getElementById(
    "settings-server-name",
  ) as HTMLInputElement | null;
  if (nameInput) nameInput.value = state.config?.serverName || "";
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
}

function escapeHtml(value: string): string {
  return value.replace(/[&<>"']/g, (char) => {
    switch (char) {
      case "&":
        return "&amp;";
      case "<":
        return "&lt;";
      case ">":
        return "&gt;";
      case '"':
        return "&quot;";
      case "'":
        return "&#39;";
      default:
        return char;
    }
  });
}

function settingsSectionFor(childId: string): HTMLElement | null {
  return document
    .getElementById(childId)
    ?.closest("section") as HTMLElement | null;
}

function styleSettingsSection(section: HTMLElement | null, id: string): void {
  if (!section) return;
  section.id = id;
  section.className = "dashboard-section space-y-5 p-5";
  section.querySelector("h2")?.classList.add("dashboard-section-title");
}

function settingsNavHtml(id = ""): string {
  return `<nav${id ? ` id="${id}"` : ""} class="dashboard-nav-strip w-full" aria-label="Settings sections">
      <div class="flex flex-wrap gap-2">
          <a class="btn btn-sm btn-ghost" href="#server-settings-section">Server</a>
          <a class="btn btn-sm btn-ghost" href="#recording-settings-section">Recording</a>
          <a class="btn btn-sm btn-ghost" href="#srt-settings-section">SRT</a>
          <a class="btn btn-sm btn-ghost" href="#backend-policy-section">Backend</a>
          <a class="btn btn-sm btn-ghost" href="#transcode-profiles-section">Profiles</a>
      </div>
  </nav>`;
}

function settingsV2Active(): boolean {
  const toggle = document.getElementById("dashboard-ui-v2-toggle");
  if (toggle instanceof HTMLInputElement && toggle.checked) return true;
  try {
    return new URLSearchParams(window.location.search).get("ui") === "v2";
  } catch (_err) {
    return false;
  }
}

function applySettingsV2Disclosure(container: HTMLElement): void {
  if (!settingsV2Active()) return;
  const disclosures: SettingsDisclosureConfig[] = [
    {
      id: "recording-settings-section",
      title: "Recording",
      summary: "Retention policy for completed MPEG-TS to MP4 conversions.",
    },
    {
      id: "srt-settings-section",
      title: "Global SRT Ingest",
      summary: "Default encryption policy for SRT publishers.",
    },
    {
      id: "backend-policy-section",
      title: "Transcoding Backend",
      summary: "Backend selection for newly started transcoding stages.",
    },
    {
      id: "transcode-profiles-section",
      title: "Transcode Profiles",
      summary: "Encoder presets used by HEVC/H.264 and resolution workflows.",
    },
  ];

  for (const disclosure of disclosures) {
    const body = container.querySelector<HTMLElement>(`#${disclosure.id}`);
    if (!body || body.closest("[data-settings-v2-disclosure]")) continue;
    const wrapper = document.createElement("details");
    wrapper.id = disclosure.id;
    wrapper.className =
      "border-base-content/10 bg-base-100/60 rounded-lg border px-3 py-2";
    wrapper.dataset.settingsV2Disclosure = disclosure.id;
    wrapper.innerHTML = `<summary class="flex cursor-pointer list-none flex-wrap items-center justify-between gap-2">
        <span>
          <span class="text-sm font-semibold">${escapeHtml(disclosure.title)}</span>
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

function pluralize(
  count: number,
  singular: string,
  plural = `${singular}s`,
): string {
  return `${count} ${count === 1 ? singular : plural}`;
}

function countConfiguredProfiles(): number {
  const list = document.getElementById("transcode-profiles-list");
  if (!list) return Object.keys(effectiveTranscodeProfiles()).length;
  const rendered = list.querySelectorAll("[data-profile-name]").length;
  return rendered || Object.keys(effectiveTranscodeProfiles()).length;
}

function settingsSummaryText(): string {
  const serverName = state.config?.serverName || "server";
  return `${serverName} settings · ${pluralize(SETTINGS_SECTION_COUNT, "section")} · ${pluralize(countConfiguredProfiles(), "profile")} · ${pluralize(lastRateLimitAttemptCount, "auth attempt")}`;
}

function countBannedAuthAttempts(): number {
  return lastRateLimitAttempts.filter((attempt) => attempt.banned).length;
}

function filteredAuthAttemptCount(): number {
  const search = normalizeSettingsSearch(rateLimitSearchQuery);
  if (!search) return lastRateLimitAttemptCount;
  return lastRateLimitAttempts.filter((attempt) =>
    rateLimitAttemptSearchText(attempt).includes(search),
  ).length;
}

function currentAuthSearchLabel(): string {
  const query = rateLimitSearchQuery.trim();
  if (!query) return `${pluralize(lastRateLimitAttemptCount, "attempt")} visible`;
  return `${filteredAuthAttemptCount()}/${lastRateLimitAttemptCount} matched`;
}

function buildSettingsCheckpointModel(): SettingsCheckpointModel {
  const profileCount = countConfiguredProfiles();
  const bannedCount = countBannedAuthAttempts();
  const query = rateLimitSearchQuery.trim();
  return {
    authLabel: pluralize(lastRateLimitAttemptCount, "auth attempt"),
    canOpenStatus: true,
    focusLabel: query
      ? `${filteredAuthAttemptCount()} authentication attempt${filteredAuthAttemptCount() === 1 ? "" : "s"} match "${query}". Clear search before changing global rate-limit settings.`
      : bannedCount > 0
        ? `${bannedCount} authentication attempt${bannedCount === 1 ? " is" : "s are"} currently banned; review the table before resetting global limits.`
        : "Configuration sections stay grouped by operational concern; use the section rail before editing dense forms.",
    metrics: [
      { label: "Server", value: state.config?.serverName || "server" },
      {
        label: "Security",
        value: bannedCount
          ? pluralize(bannedCount, "banned attempt")
          : "No bans",
      },
      {
        label: "Ingest host",
        value: state.config?.ingestHost || "default host",
      },
    ],
    nextStep:
      bannedCount > 0
        ? "Review or reset the banned attempts, then open Status to confirm the service is healthy."
        : "Edit the needed section, save, then open Status to confirm runtime health.",
    profileLabel: pluralize(profileCount, "profile"),
    searchLabel: currentAuthSearchLabel(),
    sectionLabel: pluralize(SETTINGS_SECTION_COUNT, "section"),
    securityLabel: bannedCount
      ? pluralize(bannedCount, "banned attempt")
      : "No bans",
    statusLabel: query ? "Filtered" : bannedCount ? "Review" : "Loaded",
    statusTone: query ? "warning" : bannedCount ? "warning" : "success",
    summary: settingsSummaryText(),
    title: "Settings",
  };
}

function publishSettingsCheckpoint(): void {
  settingsCheckpointCallback?.(buildSettingsCheckpointModel());
}

export function configureSettingsCheckpointPresentation(options: {
  onPresentation?: (model: SettingsCheckpointModel | null) => void;
}): void {
  settingsCheckpointCallback = options.onPresentation || null;
  if (settingsCheckpointCallback) {
    settingsCheckpointCallback(buildSettingsCheckpointModel());
  }
}

function updateSettingsSummary(): void {
  const summary = document.getElementById("settings-route-summary");
  if (!summary) return;
  summary.textContent = settingsSummaryText();
  publishSettingsCheckpoint();
}

function normalizeSettingsSearch(value: string): string {
  return value.trim().toLowerCase();
}

function rateLimitAttemptSearchText(attempt: RateLimitAttempt): string {
  return [
    attempt.scope,
    formatRateLimitScope(attempt.scope),
    attempt.ip,
    attempt.failureCount,
    formatBanStatus(attempt),
  ]
    .filter((value) => value !== null && value !== undefined && value !== "")
    .join(" ")
    .toLowerCase();
}

function authAttemptsSearchSummaryText(
  shownCount: number,
  totalCount: number,
  query: string,
  visibleCount = shownCount,
): string {
  const trimmed = query.trim();
  if (!trimmed) {
    if (shownCount > AUTH_ATTEMPT_VISIBLE_LIMIT) {
      return `${pluralize(visibleCount, "auth attempt")} shown of ${shownCount}`;
    }
    return `${pluralize(totalCount, "auth attempt")} visible`;
  }
  return `${shownCount}/${totalCount} auth attempts match "${trimmed}"`;
}

function updateAuthAttemptsSearchSummary(
  shownCount: number,
  totalCount: number,
  visibleCount = shownCount,
): void {
  const summary = document.getElementById("auth-attempts-search-summary");
  if (!summary) return;
  summary.textContent = authAttemptsSearchSummaryText(
    shownCount,
    totalCount,
    rateLimitSearchQuery,
    visibleCount,
  );
}

function syncSettingsAccountActions(container: ParentNode = document): void {
  const toggle = container.querySelector<HTMLButtonElement>(
    "#settings-account-actions-toggle",
  );
  const logoutButton = container.querySelector<HTMLButtonElement>(
    "#settings-logout-btn",
  );
  const v2 = settingsV2Active();
  toggle?.classList.toggle("hidden", !v2);
  toggle?.setAttribute(
    "aria-expanded",
    settingsAccountActionsExpanded ? "true" : "false",
  );
  if (toggle) {
    toggle.textContent = settingsAccountActionsExpanded
      ? "Hide account actions"
      : "Show account actions";
  }
  logoutButton?.classList.toggle(
    "hidden",
    v2 && !settingsAccountActionsExpanded,
  );
}

function ensureSettingsNav(container: Element): void {
  if (document.getElementById("settings-admin-nav")) return;
  const title = container.querySelector("h1");
  const header = title?.closest(".flex");
  header?.insertAdjacentHTML("afterend", settingsNavHtml("settings-admin-nav"));
}

function applySettingsChrome(): void {
  const container = document.querySelector(".flex-1.overflow-y-auto > div");
  if (container instanceof HTMLElement) {
    container.className = "dashboard-page-shell";
    const title = container.querySelector("h1");
    if (title) {
      title.textContent = "Admin";
      title.className = "dashboard-title";
    }
    ensureSettingsNav(container);
  }

  const serverSection = settingsSectionFor("settings-server-name");
  styleSettingsSection(serverSection, "server-settings-section");
  const profilesSection = document.getElementById(
    "transcode-profiles-list",
  )?.parentElement;
  if (profilesSection instanceof HTMLElement)
    profilesSection.id = "transcode-profiles-section";
}

export function registerSettingsGlobals(): void {
  window.saveServerName = saveServerName;
  window.saveIngestHost = saveIngestHost;
  window.saveIngestSecurity = saveIngestSecurity;
  window.saveRecordingSettings = saveRecordingSettings;
  window.saveSrtIngest = saveSrtIngest;
  window.saveBackendPolicy = saveBackendPolicy;
  window.saveTranscodeProfiles = saveTranscodeProfiles;
  window.addTranscodeProfile = addTranscodeProfile;
  window.saveDashboardPassword = saveDashboardPassword;
  window.dismissDashboardPasswordPrompt = dismissDashboardPasswordPrompt;
  window.refreshRateLimitState = refreshRateLimitState;
  window.resetRateLimitState = resetRateLimitStateFromUi;
  window.logoutUser = logoutUser;
}

export function renderSettingsPanel(container: HTMLElement): void {
  registerSettingsGlobals();
  container.innerHTML = `
        <div class="dashboard-page-shell">
            <div class="flex flex-wrap items-end justify-between gap-3">
                <div>
                    <h1 class="dashboard-title">Settings</h1>
                    <p class="dashboard-subtitle">Server, security, and encoding configuration.</p>
                </div>
            </div>
            <p id="settings-route-summary" class="text-base-content/60 text-sm" role="status" aria-live="polite"></p>
            ${settingsNavHtml()}

            <section id="server-settings-section" class="dashboard-section space-y-5 p-5">
                <div>
                    <h2 class="dashboard-section-title">Server</h2>
                </div>

                <div class="space-y-4">
                    <div class="max-w-2xl space-y-2">
                        <label for="settings-server-name" class="text-sm font-medium">Server Name</label>
                        <div class="flex flex-wrap items-center gap-2">
                            <input type="text" id="settings-server-name" class="input input-sm min-w-0 flex-1" placeholder="Name" />
                            <button class="btn btn-accent btn-sm" data-settings-action="save-server-name">Save</button>
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
                                placeholder="e.g. 192.168.1.10 (blank = localhost)" />
                            <button class="btn btn-accent btn-sm" data-settings-action="save-ingest-host">Save</button>
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

                <div class="space-y-2">
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
                                <button class="btn btn-accent btn-sm" data-settings-action="save-dashboard-password">Save</button>
                                <span id="dashboard-password-saved" class="text-success hidden text-sm">Saved</span>
                            </div>
                        </fieldset>
                    </div>
                </div>

                <div class="divider my-0"></div>

                <div class="space-y-2">
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
                                <button class="btn btn-accent btn-sm" data-settings-action="save-ingest-security">Save</button>
                                <span id="ingest-security-saved" class="text-success hidden text-sm">Saved</span>
                            </div>
                        </fieldset>
                    </div>
                </div>

                <div class="space-y-2">
                    <div class="flex flex-wrap items-center justify-between gap-2">
                        <div class="text-sm font-medium">Authentication Attempts</div>
                        <div class="flex items-center gap-2">
                            <button class="btn btn-ghost btn-sm" data-settings-action="refresh-rate-limits">Refresh</button>
                            <button id="auth-reset-actions-toggle" type="button" class="btn btn-outline btn-sm hidden" aria-expanded="false">Show reset actions</button>
                            <button id="auth-reset-all-btn" class="btn btn-outline btn-sm" data-settings-action="reset-rate-limits">Reset All</button>
                        </div>
                    </div>
                    <div class="flex flex-wrap items-end gap-3">
                        <label class="form-control w-full max-w-md">
                            <span class="label-text text-base-content/70">Search authentication attempts</span>
                            <input id="auth-attempts-search" class="input input-sm input-bordered mt-1" type="search" value="" placeholder="scope, IP, banned, tracking…" autocomplete="off" />
                        </label>
                        <button id="auth-attempts-clear-search-btn" type="button" class="btn btn-sm btn-outline hidden">Clear search</button>
                        <button id="auth-attempts-toggle" type="button" class="btn btn-sm btn-outline hidden" aria-expanded="false">Show all</button>
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
                                <button class="btn btn-accent btn-sm" data-settings-action="save-recording-settings">Save</button>
                                <span id="recording-settings-saved" class="text-success hidden text-sm">Saved</span>
                            </div>
                        </fieldset>
                    </div>
                </div>

                <div class="divider my-0"></div>

                <div id="srt-settings-section" class="space-y-2">
                    <div class="text-sm font-medium">Global SRT Ingest</div>
                    <p class="text-base-content/60 text-sm">Default listener policy for SRT publishers. Pipelines can inherit this, force plaintext, or use their own passphrase.</p>
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
                            <legend class="fieldset-legend invisible">_</legend>
                            <div class="flex items-center gap-3">
                                <button class="btn btn-accent btn-sm" data-settings-action="save-srt-ingest">Save</button>
                                <span id="srt-ingest-saved" class="text-success hidden text-sm">Saved</span>
                            </div>
                        </fieldset>
                    </div>
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
                        <button class="btn btn-accent btn-sm" data-settings-action="save-backend-policy">Save</button>
                        <span id="backend-policy-saved" class="text-success hidden text-sm">Saved</span>
                    </div>
                </div>

                <div class="divider my-0"></div>

                <div id="transcode-profiles-section" class="space-y-3">
                    <div class="flex flex-wrap items-baseline gap-3">
                        <span class="shrink-0 text-sm font-medium">Transcode Profiles</span>
                        <span class="text-sm opacity-70">Encoder settings per profile name. Used for H.265 to H.264 and resolution presets. Changes apply to new transcoder spawns.</span>
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
  applySettingsV2Disclosure(container);
  syncSettingsAccountActions(container);
  bindSettingsPanelActions(container);
}

function bindSettingsPanelActions(container: HTMLElement): void {
  const authSearch = container.querySelector<HTMLInputElement>(
    "#auth-attempts-search",
  );
  authSearch?.addEventListener("input", () => {
    const cursor = authSearch.selectionStart ?? authSearch.value.length;
    rateLimitSearchQuery = authSearch.value;
    authAttemptsExpanded = false;
    authResetActionsExpanded = false;
    renderRateLimitAttempts(lastRateLimitAttempts);
    const nextSearch = document.getElementById(
      "auth-attempts-search",
    ) as HTMLInputElement | null;
    nextSearch?.focus();
    nextSearch?.setSelectionRange(cursor, cursor);
  });
  container
    .querySelector<HTMLButtonElement>("#auth-attempts-clear-search-btn")
    ?.addEventListener("click", () => {
      rateLimitSearchQuery = "";
      authAttemptsExpanded = false;
      authResetActionsExpanded = false;
      renderRateLimitAttempts(lastRateLimitAttempts);
      const searchInput =
        document.getElementById(
          "auth-attempts-search",
        ) as HTMLInputElement | null;
      if (searchInput) {
        searchInput.value = "";
        searchInput.focus();
      }
    });
  container
    .querySelector<HTMLButtonElement>("#auth-attempts-toggle")
    ?.addEventListener("click", () => {
      authAttemptsExpanded = !authAttemptsExpanded;
      renderRateLimitAttempts(lastRateLimitAttempts);
    });
  container
    .querySelector<HTMLButtonElement>("#auth-reset-actions-toggle")
    ?.addEventListener("click", () => {
      authResetActionsExpanded = !authResetActionsExpanded;
      renderRateLimitAttempts(lastRateLimitAttempts);
      document.getElementById("auth-reset-actions-toggle")?.focus();
    });
  container
    .querySelector<HTMLButtonElement>("#settings-account-actions-toggle")
    ?.addEventListener("click", () => {
      settingsAccountActionsExpanded = !settingsAccountActionsExpanded;
      syncSettingsAccountActions(container);
      document.getElementById("settings-account-actions-toggle")?.focus();
    });
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
        void resetRateLimitStateFromUi(button.dataset.scope, button.dataset.ip);
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
}

// ── Server Name ───────────────────────────────────────

export async function saveServerName(): Promise<void> {
  const nameInput = document.getElementById(
    "settings-server-name",
  ) as HTMLInputElement | null;
  const name = nameInput?.value?.trim();
  if (!name) {
    showErrorAlert("Server name cannot be empty");
    return;
  }
  const result = await patchConfig({ serverName: name });
  if (result) {
    state.config = { ...state.config, serverName: result.serverName };
    updateSettingsSummary();
    showSavedFeedback("server-name-saved");
  }
}

// ── Ingest Host ───────────────────────────────────────

export async function saveIngestHost(): Promise<void> {
  const hostInput = document.getElementById(
    "settings-ingest-host",
  ) as HTMLInputElement | null;
  const ingestHost = hostInput?.value?.trim() ?? "";
  const result = await patchConfig({ ingestHost });
  if (result) {
    state.config = { ...state.config, ingestHost: result.ingestHost };
    if (hostInput) hostInput.value = result.ingestHost;
    showSavedFeedback("ingest-host-saved");
  }
}

// ── Dashboard Password ────────────────────────────────

export async function saveDashboardPassword(): Promise<void> {
  const currentInput = document.getElementById(
    "current-password-input",
  ) as HTMLInputElement | null;
  const newInput = document.getElementById(
    "new-password-input",
  ) as HTMLInputElement | null;
  const confirmInput = document.getElementById(
    "confirm-password-input",
  ) as HTMLInputElement | null;

  const currentPassword = currentInput?.value ?? "";
  const newPassword = newInput?.value ?? "";
  const confirmPassword = confirmInput?.value ?? "";

  if (!currentPassword || !newPassword || newPassword !== confirmPassword) {
    showErrorAlert("Enter the current password and matching new password");
    return;
  }
  if (newPassword.length < 12) {
    showErrorAlert("New password must be at least 12 characters");
    return;
  }

  const result = await changePassword(currentPassword, newPassword);
  if (!result) return;

  state.config = {
    ...state.config,
    dashboardPasswordChangeRecommended: false,
  };
  syncDashboardPasswordPrompt();
  if (currentInput) currentInput.value = "";
  if (newInput) newInput.value = "";
  if (confirmInput) confirmInput.value = "";
  showSavedFeedback("dashboard-password-saved");
}

export async function dismissDashboardPasswordPrompt(): Promise<void> {
  const result = await dismissPasswordChangePrompt();
  if (!result) return;
  state.config = {
    ...state.config,
    dashboardPasswordChangeRecommended: false,
  };
  syncDashboardPasswordPrompt();
}

export async function logoutUser(): Promise<void> {
  await logout();
  window.location.href = withBasePath("/login");
}

// ── Ingest Security ───────────────────────────────────

function getNumberInputValue(id: string): number | null {
  const input = document.getElementById(id) as HTMLInputElement | null;
  const value = Number(input?.value);
  if (!Number.isFinite(value) || value < 1) return null;
  return Math.floor(value);
}

function setNumberInputValue(id: string, value: number | undefined): void {
  const input = document.getElementById(id) as HTMLInputElement | null;
  if (!input || value === undefined) return;
  input.value = String(value);
}

function populateIngestSecuritySettings(): void {
  const cfg = state.config?.ingestSecurity;
  if (!cfg) return;
  setNumberInputValue("ingest-security-failure-limit", cfg.failureLimit);
  setNumberInputValue("ingest-security-failure-window-ms", cfg.failureWindowMs);
  setNumberInputValue("ingest-security-ban-ms", cfg.banMs);
  setNumberInputValue("ingest-security-tracked-ip-limit", cfg.trackedIpLimit);
}

export async function saveIngestSecurity(): Promise<void> {
  const failureLimit = getNumberInputValue("ingest-security-failure-limit");
  const failureWindowMs = getNumberInputValue(
    "ingest-security-failure-window-ms",
  );
  const banMs = getNumberInputValue("ingest-security-ban-ms");
  const trackedIpLimit = getNumberInputValue(
    "ingest-security-tracked-ip-limit",
  );

  if (!failureLimit || !failureWindowMs || !banMs || !trackedIpLimit) {
    showErrorAlert("Ingest security values must be positive numbers");
    return;
  }

  const result = await patchConfig({
    ingestSecurity: { failureLimit, failureWindowMs, banMs, trackedIpLimit },
  });
  if (result) {
    state.config = { ...state.config, ingestSecurity: result.ingestSecurity };
    populateIngestSecuritySettings();
    showSavedFeedback("ingest-security-saved");
  }
}

function formatRateLimitScope(scope: string): string {
  switch (scope) {
    case "dashboard-login":
      return "Dashboard";
    case "rtmp-publish":
      return "RTMP publish";
    case "srt-publish":
      return "SRT publish";
    case "srt-read":
      return "SRT read";
    default:
      return scope;
  }
}

function formatBanStatus(attempt: RateLimitAttempt): string {
  if (!attempt.banned) return "Tracking";
  const remainingMs = attempt.banRemainingMs ?? 0;
  const seconds = Math.ceil(remainingMs / 1000);
  return seconds > 0 ? `Banned ${seconds}s` : "Banned";
}

function renderRateLimitAttempts(attempts: RateLimitAttempt[]): void {
  const body = document.getElementById("rate-limit-attempts-body");
  if (!body) return;
  lastRateLimitAttemptCount = attempts.length;
  lastRateLimitAttempts = attempts;
  const search = normalizeSettingsSearch(rateLimitSearchQuery);
  const shownAttempts = attempts.filter(
    (attempt) => !search || rateLimitAttemptSearchText(attempt).includes(search),
  );
  const showToggle = !search && shownAttempts.length > AUTH_ATTEMPT_VISIBLE_LIMIT;
  const visibleAttempts =
    showToggle && !authAttemptsExpanded
      ? shownAttempts.slice(0, AUTH_ATTEMPT_VISIBLE_LIMIT)
      : shownAttempts;
  const resetActionsVisible = !settingsV2Active() || authResetActionsExpanded;
  const resetActionsToggle = document.getElementById(
    "auth-reset-actions-toggle",
  ) as HTMLButtonElement | null;
  const resetAllButton = document.getElementById(
    "auth-reset-all-btn",
  ) as HTMLButtonElement | null;
  const resetHeading = document.getElementById(
    "auth-attempts-reset-heading",
  ) as HTMLTableCellElement | null;
  resetActionsToggle?.classList.toggle("hidden", !settingsV2Active());
  resetActionsToggle?.setAttribute(
    "aria-expanded",
    authResetActionsExpanded ? "true" : "false",
  );
  if (resetActionsToggle) {
    resetActionsToggle.textContent = authResetActionsExpanded
      ? "Hide reset actions"
      : "Show reset actions";
  }
  resetAllButton?.classList.toggle("hidden", !resetActionsVisible);
  resetHeading?.classList.toggle("hidden", !resetActionsVisible);
  document
    .getElementById("auth-attempts-clear-search-btn")
    ?.classList.toggle("hidden", !search);
  const toggle = document.getElementById(
    "auth-attempts-toggle",
  ) as HTMLButtonElement | null;
  if (toggle) {
    toggle.classList.toggle("hidden", !showToggle);
    toggle.setAttribute(
      "aria-expanded",
      authAttemptsExpanded ? "true" : "false",
    );
    toggle.textContent = authAttemptsExpanded
      ? "Show fewer"
      : `Show all ${shownAttempts.length}`;
  }
  if (visibleAttempts.length === 0) {
    body.innerHTML = `<tr><td colspan="${resetActionsVisible ? "5" : "4"}" class="text-base-content/60">${search ? `No authentication attempts match "${escapeHtml(rateLimitSearchQuery.trim())}". Clear search to return to the full security log.` : "No attempts"}</td></tr>`;
    updateAuthAttemptsSearchSummary(
      shownAttempts.length,
      attempts.length,
      visibleAttempts.length,
    );
    updateSettingsSummary();
    return;
  }
  body.innerHTML = visibleAttempts
    .map((attempt) => {
      return `
        <tr>
          <td>${escapeHtml(formatRateLimitScope(attempt.scope))}</td>
          <td><code>${escapeHtml(attempt.ip)}</code></td>
          <td>${attempt.failureCount}</td>
          <td>${escapeHtml(formatBanStatus(attempt))}</td>
          ${
            resetActionsVisible
              ? `<td class="text-right">
                  <button
                    class="btn btn-ghost btn-xs"
                    data-settings-action="reset-rate-limits"
                    data-scope="${escapeHtml(attempt.scope)}"
                    data-ip="${escapeHtml(attempt.ip)}">Reset</button>
                </td>`
              : ""
          }
        </tr>`;
    })
    .join("");
  updateAuthAttemptsSearchSummary(
    shownAttempts.length,
    attempts.length,
    visibleAttempts.length,
  );
  updateSettingsSummary();
}

export async function refreshRateLimitState(): Promise<void> {
  const result = await getRateLimitState();
  if (!result) return;
  renderRateLimitAttempts(result.attempts);
}

export async function resetRateLimitStateFromUi(
  scope?: string,
  ip?: string,
): Promise<void> {
  const result = await resetRateLimitState({
    ...(scope ? { scope } : {}),
    ...(ip ? { ip } : {}),
  });
  if (!result) return;
  await refreshRateLimitState();
}

function effectiveRecordingSettings(): RecordingSettings {
  return {
    retainSourceTs: state.config?.recordingSettings?.retainSourceTs ?? false,
  };
}

function populateRecordingSettings(): void {
  const input = document.getElementById(
    "recording-retain-source-ts",
  ) as HTMLInputElement | null;
  if (!input) return;
  input.checked = effectiveRecordingSettings().retainSourceTs;
}

export async function saveRecordingSettings(): Promise<void> {
  const input = document.getElementById(
    "recording-retain-source-ts",
  ) as HTMLInputElement | null;
  const recordingSettings: RecordingSettings = {
    retainSourceTs: input?.checked ?? false,
  };
  const result = await patchConfig({ recordingSettings });
  if (result) {
    state.config = {
      ...state.config,
      recordingSettings: result.recordingSettings,
    };
    populateRecordingSettings();
    showSavedFeedback("recording-settings-saved");
  }
}

function showSavedFeedback(id: string): void {
  const el = document.getElementById(id);
  if (!el) return;
  el.classList.remove("hidden");
  setTimeout(() => el.classList.add("hidden"), 2000);
}

function syncDashboardPasswordPrompt(): void {
  const prompt = document.getElementById("dashboard-password-prompt");
  if (!prompt) return;
  const show = state.config?.dashboardPasswordChangeRecommended === true;
  prompt.classList.toggle("hidden", !show);
  prompt.classList.toggle("flex", show);
}

function getSrtPbkeylenInputValue(id: string): 16 | 24 | 32 {
  const value = Number(
    (document.getElementById(id) as HTMLSelectElement | null)?.value || 16,
  );
  return value === 24 || value === 32 ? value : 16;
}

function setSrtModeUi(mode: "plaintext" | "encrypted"): void {
  const passphraseInput = document.getElementById(
    "srt-ingest-passphrase-input",
  ) as HTMLInputElement | null;
  const pbkeylenInput = document.getElementById(
    "srt-ingest-pbkeylen-input",
  ) as HTMLSelectElement | null;
  const encrypted = mode === "encrypted";
  if (passphraseInput) {
    passphraseInput.disabled = !encrypted;
    passphraseInput.classList.toggle("input-disabled", !encrypted);
  }
  if (pbkeylenInput) {
    pbkeylenInput.disabled = !encrypted;
    pbkeylenInput.classList.toggle("select-disabled", !encrypted);
  }
}

function populateSrtIngestSettings(): void {
  const cfg = state.config?.srtIngest || { mode: "plaintext", pbkeylen: 16 };
  const modeInput = document.getElementById(
    "srt-ingest-mode-input",
  ) as HTMLSelectElement | null;
  const passphraseInput = document.getElementById(
    "srt-ingest-passphrase-input",
  ) as HTMLInputElement | null;
  const pbkeylenInput = document.getElementById(
    "srt-ingest-pbkeylen-input",
  ) as HTMLSelectElement | null;
  if (modeInput) {
    modeInput.value = cfg.mode;
    modeInput.onchange = () =>
      setSrtModeUi(modeInput.value === "encrypted" ? "encrypted" : "plaintext");
  }
  if (passphraseInput) passphraseInput.value = cfg.passphrase || "";
  if (pbkeylenInput) pbkeylenInput.value = String(cfg.pbkeylen || 16);
  setSrtModeUi(cfg.mode === "encrypted" ? "encrypted" : "plaintext");
}

function readSrtIngestSettings(): SrtGlobalIngestConfig | null {
  const mode =
    (
      document.getElementById(
        "srt-ingest-mode-input",
      ) as HTMLSelectElement | null
    )?.value === "encrypted"
      ? "encrypted"
      : "plaintext";
  const passphrase =
    (
      document.getElementById(
        "srt-ingest-passphrase-input",
      ) as HTMLInputElement | null
    )?.value.trim() || "";
  const pbkeylen = getSrtPbkeylenInputValue("srt-ingest-pbkeylen-input");
  if (
    mode === "encrypted" &&
    (passphrase.length < 10 || passphrase.length > 79)
  ) {
    showErrorAlert("SRT passphrase must be 10-79 bytes");
    return null;
  }
  return {
    mode,
    passphrase: mode === "encrypted" ? passphrase : null,
    pbkeylen,
  };
}

export async function saveSrtIngest(): Promise<void> {
  const srtIngest = readSrtIngestSettings();
  if (!srtIngest) return;
  const result = await patchConfig({ srtIngest });
  if (result) {
    state.config = { ...state.config, srtIngest: result.srtIngest };
    populateSrtIngestSettings();
    showSavedFeedback("srt-ingest-saved");
  }
}

// ── Transcoding Backend ───────────────────────────────

const DEFAULT_BACKEND_POLICY: BackendPolicy = {
  internalVideoPresets: false,
  internalHevcToH264: false,
  internalHlsPreview: false,
  internalComplexAudio: false,
};

function effectiveBackendPolicy(): BackendPolicy {
  return {
    ...DEFAULT_BACKEND_POLICY,
    ...(state.config?.backendPolicy ?? {}),
  };
}

function backendPolicyCheckbox(id: string): HTMLInputElement | null {
  return document.getElementById(id) as HTMLInputElement | null;
}

function populateBackendPolicySettings(): void {
  const policy = effectiveBackendPolicy();
  const inputs: Array<[string, boolean]> = [
    ["backend-policy-internal-video-presets", policy.internalVideoPresets],
    ["backend-policy-internal-hevc-to-h264", policy.internalHevcToH264],
    ["backend-policy-internal-hls-preview", policy.internalHlsPreview],
    ["backend-policy-internal-complex-audio", policy.internalComplexAudio],
  ];
  for (const [id, checked] of inputs) {
    const input = backendPolicyCheckbox(id);
    if (input) input.checked = checked;
  }
}

function readBackendPolicySettings(): BackendPolicy {
  return {
    internalVideoPresets:
      backendPolicyCheckbox("backend-policy-internal-video-presets")?.checked ??
      false,
    internalHevcToH264:
      backendPolicyCheckbox("backend-policy-internal-hevc-to-h264")?.checked ??
      false,
    internalHlsPreview:
      backendPolicyCheckbox("backend-policy-internal-hls-preview")?.checked ??
      false,
    internalComplexAudio:
      backendPolicyCheckbox("backend-policy-internal-complex-audio")?.checked ??
      false,
  };
}

export async function saveBackendPolicy(): Promise<void> {
  const backendPolicy = readBackendPolicySettings();
  const result = await patchConfig({ backendPolicy });
  if (result) {
    state.config = {
      ...state.config,
      backendPolicy: result.backendPolicy ?? backendPolicy,
    };
    populateBackendPolicySettings();
    showSavedFeedback("backend-policy-saved");
  }
}

// ── Transcode Profiles ─────────────────────────────────

const PRESET_OPTIONS = [
  "ultrafast",
  "superfast",
  "veryfast",
  "faster",
  "fast",
  "medium",
  "slow",
  "slower",
];
const TUNE_OPTIONS = [
  "zerolatency",
  "fastdecode",
  "film",
  "animation",
  "grain",
  "stillimage",
  "psnr",
  "ssim",
];
const BUILT_IN_PROFILE_ORDER = ["h264", "720p", "1080p"];
const profileTuningRowsExpanded = new Set<string>();
const BUILT_IN_TRANSCODE_PROFILES: TranscodeProfiles = {
  h264: {
    preset: "ultrafast",
    tune: "zerolatency",
    crf: 23,
    gop: 60,
    bframes: 0,
    bitrate: 0,
    maxBitrate: 0,
    width: 0,
    height: 0,
  },
  "720p": {
    preset: "ultrafast",
    tune: "zerolatency",
    crf: 23,
    gop: 60,
    bframes: 0,
    bitrate: 0,
    maxBitrate: 0,
    width: 1280,
    height: 720,
  },
  "1080p": {
    preset: "ultrafast",
    tune: "zerolatency",
    crf: 23,
    gop: 60,
    bframes: 0,
    bitrate: 0,
    maxBitrate: 0,
    width: 1920,
    height: 1080,
  },
};

function effectiveTranscodeProfiles(): TranscodeProfiles {
  return {
    ...BUILT_IN_TRANSCODE_PROFILES,
    ...(state.config?.transcodeProfiles ?? {}),
  };
}

function renderProfileRow(name: string, profile: TranscodeProfile): string {
  const presetOpts = PRESET_OPTIONS.map(
    (p) =>
      `<option value="${p}" ${profile.preset === p ? "selected" : ""}>${p}</option>`,
  ).join("");
  const tuneOpts = TUNE_OPTIONS.map(
    (t) =>
      `<option value="${t}" ${profile.tune === t ? "selected" : ""}>${t}</option>`,
  ).join("");
  const safeName = escapeHtml(name);
  const isBuiltIn = BUILT_IN_PROFILE_ORDER.includes(name);
  const v2 = settingsV2Active();
  const tuningExpanded = !v2 || profileTuningRowsExpanded.has(name);
  const tuningToggle = v2
    ? `<button type="button" class="btn btn-sm btn-outline js-profile-tuning-toggle" data-name="${safeName}" aria-expanded="${tuningExpanded ? "true" : "false"}">${tuningExpanded ? "Hide tuning" : "Show tuning"}</button>`
    : "";
  const deleteButton = isBuiltIn
    ? '<button class="btn btn-sm btn-ghost" disabled>Built-in</button>'
    : `<button class="btn btn-sm btn-error btn-outline js-profile-delete" data-name="${safeName}">Delete</button>`;
  return `
        <div class="border-base-content/10 bg-base-100 space-y-3 rounded-lg border px-3 py-3" data-profile-name="${safeName}" data-profile-crf="${profile.crf}" data-profile-gop="${profile.gop}" data-profile-bframes="${profile.bframes}" data-profile-bitrate="${profile.bitrate}" data-profile-max-bitrate="${profile.maxBitrate}" data-profile-width="${profile.width}" data-profile-height="${profile.height}">
            <div class="flex flex-wrap items-end gap-2">
                <fieldset class="fieldset">
                    <legend class="fieldset-legend">Name</legend>
                    <input type="text" class="input input-sm w-36 font-mono js-profile-name" value="${safeName}" placeholder="profile name" ${isBuiltIn ? "readonly" : ""} />
                </fieldset>
                <fieldset class="fieldset">
                    <legend class="fieldset-legend">Preset</legend>
                <select class="select select-sm js-profile-preset" aria-label="${safeName} preset">${presetOpts}</select>
                </fieldset>
                <fieldset class="fieldset">
                    <legend class="fieldset-legend">Tune</legend>
                <select class="select select-sm js-profile-tune" aria-label="${safeName} tune">${tuneOpts}</select>
                </fieldset>
                ${tuningToggle}
                ${deleteButton}
            </div>
            <div data-profile-tuning="${safeName}">${tuningExpanded ? renderProfileTuningFields(profile) : ""}</div>
        </div>`;
}

function renderProfileTuningFields(profile: TranscodeProfile): string {
  return `<div class="grid gap-2 text-sm sm:grid-cols-2 lg:grid-cols-4">
        <label class="flex items-center gap-2">CRF <input type="number" class="input input-xs w-full js-profile-crf" value="${profile.crf}" min="0" max="51" /></label>
        <label class="flex items-center gap-2">GOP <input type="number" class="input input-xs w-full js-profile-gop" value="${profile.gop}" min="1" /></label>
        <label class="flex items-center gap-2">B-frames <input type="number" class="input input-xs w-full js-profile-bframes" value="${profile.bframes}" min="0" /></label>
        <label class="flex items-center gap-2">Bitrate <input type="number" class="input input-xs w-full js-profile-bitrate" value="${profile.bitrate}" placeholder="0=CRF" /></label>
        <label class="flex items-center gap-2">Max <input type="number" class="input input-xs w-full js-profile-maxbitrate" value="${profile.maxBitrate}" placeholder="0=none" /></label>
        <label class="flex items-center gap-2">Width <input type="number" class="input input-xs w-full js-profile-width" value="${profile.width}" placeholder="0=src" /></label>
        <label class="flex items-center gap-2">Height <input type="number" class="input input-xs w-full js-profile-height" value="${profile.height}" placeholder="0=src" /></label>
    </div>`;
}

function profileNumber(row: HTMLElement, selector: string, key: string): number {
  const input = row.querySelector<HTMLInputElement>(selector);
  const raw = input?.value ?? row.dataset[key] ?? "";
  return Number(raw);
}

function profileFromRow(row: HTMLElement): TranscodeProfile {
  return {
    preset:
      row.querySelector<HTMLSelectElement>(".js-profile-preset")?.value ||
      "ultrafast",
    tune:
      row.querySelector<HTMLSelectElement>(".js-profile-tune")?.value ||
      "zerolatency",
    crf: profileNumber(row, ".js-profile-crf", "profileCrf") || 23,
    gop: profileNumber(row, ".js-profile-gop", "profileGop") || 60,
    bframes: profileNumber(row, ".js-profile-bframes", "profileBframes") || 0,
    bitrate: profileNumber(row, ".js-profile-bitrate", "profileBitrate") || 0,
    maxBitrate:
      profileNumber(row, ".js-profile-maxbitrate", "profileMaxBitrate") || 0,
    width: profileNumber(row, ".js-profile-width", "profileWidth") || 0,
    height: profileNumber(row, ".js-profile-height", "profileHeight") || 0,
  };
}

function syncProfileTuningDataset(row: HTMLElement): void {
  const profile = profileFromRow(row);
  row.dataset.profileCrf = String(profile.crf);
  row.dataset.profileGop = String(profile.gop);
  row.dataset.profileBframes = String(profile.bframes);
  row.dataset.profileBitrate = String(profile.bitrate);
  row.dataset.profileMaxBitrate = String(profile.maxBitrate);
  row.dataset.profileWidth = String(profile.width);
  row.dataset.profileHeight = String(profile.height);
}

function bindProfileTuningToggles(root: ParentNode): void {
  root
    .querySelectorAll<HTMLButtonElement>(".js-profile-tuning-toggle")
    .forEach((btn) => {
      btn.addEventListener("click", () => {
        const row = btn.closest<HTMLElement>("[data-profile-name]");
        const name = row?.dataset.profileName || btn.dataset.name || "";
        const tuning = row?.querySelector<HTMLElement>("[data-profile-tuning]");
        if (!row || !tuning || !name) return;
        const expanded = !profileTuningRowsExpanded.has(name);
        if (expanded) {
          profileTuningRowsExpanded.add(name);
          tuning.innerHTML = renderProfileTuningFields(profileFromRow(row));
        } else {
          syncProfileTuningDataset(row);
          profileTuningRowsExpanded.delete(name);
          tuning.innerHTML = "";
        }
        btn.setAttribute("aria-expanded", expanded ? "true" : "false");
        btn.textContent = expanded ? "Hide tuning" : "Show tuning";
      });
    });
}

export function loadTranscodeProfiles(): void {
  const list = document.getElementById("transcode-profiles-list");
  if (!list) return;
  const profiles = effectiveTranscodeProfiles();
  const entries = Object.entries(profiles).sort(([a], [b]) => {
    const ai = BUILT_IN_PROFILE_ORDER.indexOf(a);
    const bi = BUILT_IN_PROFILE_ORDER.indexOf(b);
    if (ai !== -1 || bi !== -1) {
      if (ai === -1) return 1;
      if (bi === -1) return -1;
      return ai - bi;
    }
    return a.localeCompare(b);
  });
  if (entries.length === 0) {
    list.innerHTML =
      '<div class="border-base-content/10 bg-base-100 rounded-lg border px-3 py-4 text-sm opacity-70">No profiles configured. Using built-in defaults.</div>';
    return;
  }
  list.innerHTML = entries
    .map(([name, p]) => renderProfileRow(name, p))
    .join("");
  updateSettingsSummary();
  bindProfileTuningToggles(list);
  list
    .querySelectorAll<HTMLButtonElement>(".js-profile-delete")
    .forEach((btn) => {
      btn.addEventListener("click", () => {
        const row = btn.closest("[data-profile-name]");
        if (row) {
          row.remove();
          updateSettingsSummary();
        }
      });
    });
}

export function addTranscodeProfile(): void {
  const list = document.getElementById("transcode-profiles-list");
  if (!list) return;
  if (!list.querySelector("[data-profile-name]")) {
    list.innerHTML = "";
  }
  const existing = new Set(
    Array.from(list.querySelectorAll<HTMLInputElement>(".js-profile-name")).map(
      (input) => input.value.trim(),
    ),
  );
  let nextName = "new_profile";
  let suffix = 2;
  while (existing.has(nextName)) {
    nextName = `new_profile_${suffix}`;
    suffix += 1;
  }
  if (settingsV2Active()) {
    profileTuningRowsExpanded.add(nextName);
  }
  const div = document.createElement("div");
  div.innerHTML = renderProfileRow(nextName, {
    preset: "ultrafast",
    tune: "zerolatency",
    crf: 23,
    gop: 60,
    bframes: 0,
    bitrate: 0,
    maxBitrate: 0,
    width: 0,
    height: 0,
  });
  const row = div.firstElementChild as HTMLElement | null;
  if (row) {
    list.appendChild(row);
    updateSettingsSummary();
    bindProfileTuningToggles(row);
    row
      .querySelector<HTMLButtonElement>(".js-profile-delete")
      ?.addEventListener("click", () => {
        row.remove();
        updateSettingsSummary();
      });
  }
}

export async function saveTranscodeProfiles(): Promise<void> {
  const list = document.getElementById("transcode-profiles-list");
  if (!list) return;
  const profiles: TranscodeProfiles = {};
  list.querySelectorAll<HTMLElement>("[data-profile-name]").forEach((row) => {
    const name = (
      row.querySelector(".js-profile-name") as HTMLInputElement
    )?.value?.trim();
    if (!name) return;
    profiles[name] = profileFromRow(row);
  });
  const result = await patchConfig({ transcodeProfiles: profiles });
  if (result) {
    state.config = {
      ...state.config,
      transcodeProfiles: result.transcodeProfiles,
    };
    loadTranscodeProfiles();
    showSavedFeedback("transcode-profiles-saved");
  }
}
