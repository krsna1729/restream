import {
  changePassword,
  dismissPasswordChangePrompt,
  getRateLimitState,
  logout,
  patchConfig,
  resetRateLimitState,
} from "../../core/api.js";
import { state } from "../../core/state.js";
import { escapeHtml, showErrorAlert } from "../../core/utils.js";

const AUTH_ATTEMPT_VISIBLE_LIMIT = 8;
let lastRateLimitAttemptCount = 0;
let lastRateLimitAttempts: any[] = [];
let rateLimitSearchQuery = "";

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
  const rateLimit = (await getRateLimitState()) as any;
  if (!rateLimit) return;
  lastRateLimitAttemptCount = rateLimit.failedCount || 0;
  lastRateLimitAttempts = rateLimit.recentAttempts || [];
  renderRateLimitState();
}

function renderRateLimitState(): void {
  const badge = document.getElementById("settings-rate-limit-badge");
  const countEl = document.getElementById("settings-rate-limit-count");
  const listEl = document.getElementById("settings-rate-limit-list");

  if (badge) {
    badge.textContent = `${lastRateLimitAttemptCount} failed`;
    badge.className = lastRateLimitAttemptCount > 0 ? "badge badge-sm badge-warning" : "badge badge-sm badge-ghost";
  }
  if (countEl) {
    countEl.textContent = String(lastRateLimitAttemptCount);
  }
  if (!listEl) return;

  const filtered = lastRateLimitAttempts.filter((a) => {
    if (!rateLimitSearchQuery) return true;
    const q = rateLimitSearchQuery.toLowerCase();
    return (
      (a.ip && a.ip.toLowerCase().includes(q)) ||
      (a.username && a.username.toLowerCase().includes(q)) ||
      (a.reason && a.reason.toLowerCase().includes(q))
    );
  });

  if (filtered.length === 0) {
    listEl.innerHTML = '<div class="px-3 py-4 text-xs opacity-60">No failed authentication attempts logged.</div>';
    return;
  }

  const visible = filtered.slice(0, AUTH_ATTEMPT_VISIBLE_LIMIT);
  listEl.innerHTML = visible
    .map(
      (a) => `
    <div class="border-base-content/10 bg-base-200/40 flex items-center justify-between border-b px-3 py-2 text-xs">
      <div>
        <span class="font-mono font-bold">${escapeHtml(a.ip || "unknown")}</span>
        ${a.username ? `<span class="opacity-70"> (${escapeHtml(a.username)})</span>` : ""}
      </div>
      <div class="flex items-center gap-2">
        <span class="text-error font-mono">${escapeHtml(a.reason || "invalid_credentials")}</span>
        <span class="opacity-50">${a.timestamp ? new Date(a.timestamp).toLocaleTimeString() : "--"}</span>
      </div>
    </div>`,
    )
    .join("");
}

export async function resetRateLimitStateFromUi(scope: "all" | "ip" | "username", value?: string): Promise<void> {
  const res = await resetRateLimitState({ scope: scope as any });
  if (res) {
    void refreshRateLimitState();
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
