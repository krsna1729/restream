import {
  changePassword,
  dismissPasswordChangePrompt,
  getRateLimitState,
  logout,
  patchConfig,
  resetRateLimitState,
  type RateLimitAttempt,
} from "../../core/api.js";
import { state } from "../../core/state.js";
import { escapeHtml, showErrorAlert } from "../../core/utils.js";
import { settingsV2Active } from "./presentation-mode.js";

const AUTH_ATTEMPT_VISIBLE_LIMIT = 8;
let lastRateLimitAttemptCount = 0;
let lastRateLimitAttempts: RateLimitAttempt[] = [];
let rateLimitSearchQuery = "";
let authAttemptsExpanded = false;
let authResetActionsExpanded = false;
let onRateLimitStateChange: (() => void) | null = null;

export interface SettingsRateLimitPresentation {
  readonly bannedCount: number;
  readonly filteredCount: number;
  readonly query: string;
  readonly searchLabel: string;
  readonly totalCount: number;
}

function showSavedFeedback(id: string): void {
  const el = document.getElementById(id);
  if (!el) return;
  el.classList.remove("hidden");
  setTimeout(() => el.classList.add("hidden"), 3000);
}

export function populateIngestSecuritySettings(): void {
  const modeSelect = document.getElementById("settings-ingest-auth-mode") as HTMLSelectElement | null;
  const staticKeyInput = document.getElementById("settings-ingest-static-key") as HTMLInputElement | null;
  const mode = (state.config as any)?.ingestSecurity?.mode || "none";
  if (modeSelect) modeSelect.value = mode;
  if (staticKeyInput) staticKeyInput.value = (state.config as any)?.ingestSecurity?.staticKey || "";
}

export async function saveIngestSecurity(): Promise<void> {
  const modeSelect = document.getElementById("settings-ingest-auth-mode") as HTMLSelectElement | null;
  const staticKeyInput = document.getElementById("settings-ingest-static-key") as HTMLInputElement | null;
  const mode = (modeSelect?.value || "none") as "none" | "static_key";
  const staticKey = staticKeyInput?.value || "";

  const res = await patchConfig({
    ingestSecurity: { mode, staticKey } as any,
  });
  if (res) {
    state.config = {
      ...state.config,
      ...res,
    };
    showSavedFeedback("ingest-security-saved");
  }
}

export async function refreshRateLimitState(): Promise<void> {
  const rateLimit = await getRateLimitState();
  if (!rateLimit) return;
  renderRateLimitAttempts(rateLimit.attempts || []);
}

export function setSettingsRateLimitStateChangeHandler(
  callback: (() => void) | null,
): void {
  onRateLimitStateChange = callback;
}

export function settingsRateLimitPresentation(): SettingsRateLimitPresentation {
  const query = rateLimitSearchQuery.trim();
  return {
    bannedCount: lastRateLimitAttempts.filter((attempt) => attempt.banned)
      .length,
    filteredCount: filteredAuthAttemptCount(),
    query,
    searchLabel: query
      ? `${filteredAuthAttemptCount()}/${lastRateLimitAttemptCount} matched`
      : `${pluralize(lastRateLimitAttemptCount, "attempt")} visible`,
    totalCount: lastRateLimitAttemptCount,
  };
}

function pluralize(
  count: number,
  singular: string,
  plural = `${singular}s`,
): string {
  return `${count} ${count === 1 ? singular : plural}`;
}

function normalizeSettingsSearch(value: string): string {
  return value.trim().toLowerCase();
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

function filteredAuthAttemptCount(): number {
  const search = normalizeSettingsSearch(rateLimitSearchQuery);
  if (!search) return lastRateLimitAttemptCount;
  return lastRateLimitAttempts.filter((attempt) =>
    rateLimitAttemptSearchText(attempt).includes(search),
  ).length;
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

function bindRateLimitControls(): void {
  const searchInput = document.getElementById(
    "auth-attempts-search",
  ) as HTMLInputElement | null;
  if (searchInput && searchInput.dataset.settingsRateLimitBound !== "true") {
    searchInput.dataset.settingsRateLimitBound = "true";
    searchInput.addEventListener("input", () => {
      const cursor = searchInput.selectionStart ?? searchInput.value.length;
      rateLimitSearchQuery = searchInput.value;
      authAttemptsExpanded = false;
      authResetActionsExpanded = false;
      renderRateLimitAttempts(lastRateLimitAttempts);
      const nextSearch = document.getElementById(
        "auth-attempts-search",
      ) as HTMLInputElement | null;
      nextSearch?.focus();
      nextSearch?.setSelectionRange(cursor, cursor);
    });
  }
  const clearButton = document.getElementById(
    "auth-attempts-clear-search-btn",
  ) as HTMLButtonElement | null;
  if (clearButton && clearButton.dataset.settingsRateLimitBound !== "true") {
    clearButton.dataset.settingsRateLimitBound = "true";
    clearButton.addEventListener("click", () => {
      rateLimitSearchQuery = "";
      authAttemptsExpanded = false;
      authResetActionsExpanded = false;
      renderRateLimitAttempts(lastRateLimitAttempts);
      const nextSearch = document.getElementById(
        "auth-attempts-search",
      ) as HTMLInputElement | null;
      if (nextSearch) {
        nextSearch.value = "";
        nextSearch.focus();
      }
    });
  }
  const toggle = document.getElementById(
    "auth-attempts-toggle",
  ) as HTMLButtonElement | null;
  if (toggle && toggle.dataset.settingsRateLimitBound !== "true") {
    toggle.dataset.settingsRateLimitBound = "true";
    toggle.addEventListener("click", () => {
      authAttemptsExpanded = !authAttemptsExpanded;
      renderRateLimitAttempts(lastRateLimitAttempts);
    });
  }
  const resetToggle = document.getElementById(
    "auth-reset-actions-toggle",
  ) as HTMLButtonElement | null;
  if (resetToggle && resetToggle.dataset.settingsRateLimitBound !== "true") {
    resetToggle.dataset.settingsRateLimitBound = "true";
    resetToggle.addEventListener("click", () => {
      authResetActionsExpanded = !authResetActionsExpanded;
      renderRateLimitAttempts(lastRateLimitAttempts);
      document.getElementById("auth-reset-actions-toggle")?.focus();
    });
  }
}

function renderRateLimitAttempts(attempts: readonly RateLimitAttempt[]): void {
  const body = document.getElementById("rate-limit-attempts-body");
  lastRateLimitAttemptCount = attempts.length;
  lastRateLimitAttempts = [...attempts];
  bindRateLimitControls();
  const search = normalizeSettingsSearch(rateLimitSearchQuery);
  const shownAttempts = lastRateLimitAttempts.filter(
    (attempt) => !search || rateLimitAttemptSearchText(attempt).includes(search),
  );
  const showToggle =
    !search && shownAttempts.length > AUTH_ATTEMPT_VISIBLE_LIMIT;
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
    resetActionsToggle.setAttribute(
      "aria-label",
      authResetActionsExpanded
        ? "Hide authentication reset actions"
        : "Show authentication reset actions",
    );
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
    toggle.setAttribute(
      "aria-label",
      authAttemptsExpanded
        ? "Show fewer authentication attempts"
        : `Show all ${shownAttempts.length} authentication attempts`,
    );
  }
  if (!body) {
    onRateLimitStateChange?.();
    return;
  }
  if (visibleAttempts.length === 0) {
    body.innerHTML = `<tr><td colspan="${resetActionsVisible ? "5" : "4"}" class="text-base-content/60">${search ? `No authentication attempts match "${escapeHtml(rateLimitSearchQuery.trim())}". Clear search to return to the full security log.` : "No attempts"}</td></tr>`;
    updateAuthAttemptsSearchSummary(
      shownAttempts.length,
      attempts.length,
      visibleAttempts.length,
    );
    onRateLimitStateChange?.();
    return;
  }
  body.innerHTML = visibleAttempts
    .map(
      (attempt) => `<tr>
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
                    data-ip="${escapeHtml(attempt.ip)}"
                    aria-label="Reset authentication attempt for ${escapeHtml(formatRateLimitScope(attempt.scope))} ${escapeHtml(attempt.ip)}">Reset</button>
                </td>`
              : ""
          }
        </tr>`,
    )
    .join("");
  updateAuthAttemptsSearchSummary(
    shownAttempts.length,
    attempts.length,
    visibleAttempts.length,
  );
  onRateLimitStateChange?.();
}

export async function resetRateLimitStateFromUi(scope: "all" | "ip" | "username", value?: string): Promise<void> {
  const res = await resetRateLimitState({
    ...(scope ? { scope } : {}),
    ...(value ? { ip: value } : {}),
  });
  if (res) {
    await refreshRateLimitState();
    showSavedFeedback("rate-limit-reset-saved");
  }
}

export async function saveDashboardPassword(): Promise<void> {
  const currentInput = document.getElementById("settings-current-password") as HTMLInputElement | null;
  const newInput = document.getElementById("settings-new-password") as HTMLInputElement | null;
  const confirmInput = document.getElementById("settings-confirm-password") as HTMLInputElement | null;

  const currentPassword = currentInput?.value || "";
  const newPassword = newInput?.value || "";
  const confirmPassword = confirmInput?.value || "";

  if (!newPassword) {
    showErrorAlert("New password cannot be empty");
    return;
  }
  if (newPassword !== confirmPassword) {
    showErrorAlert("New passwords do not match");
    return;
  }

  const res = await changePassword(currentPassword, newPassword);
  if (res) {
    if (currentInput) currentInput.value = "";
    if (newInput) newInput.value = "";
    if (confirmInput) confirmInput.value = "";
    showSavedFeedback("password-changed-saved");
    syncDashboardPasswordPrompt();
  }
}

export async function dismissDashboardPasswordPrompt(): Promise<void> {
  const res = await dismissPasswordChangePrompt();
  if (res) {
    syncDashboardPasswordPrompt();
  }
}

export async function logoutUser(): Promise<void> {
  await logout();
  window.location.reload();
}

export function syncDashboardPasswordPrompt(): void {
  const promptEl = document.getElementById("dashboard-password-change-prompt");
  if (!promptEl) return;

  const promptRequired = (state.config as any)?.passwordChangeRequired ?? false;
  if (promptRequired) {
    promptEl.classList.remove("hidden");
  } else {
    promptEl.classList.add("hidden");
  }
}
