import { patchConfig, type TranscodeProfile, type TranscodeProfiles } from "../../core/api.js";
import { state } from "../../core/state.js";
import { showErrorAlert } from "../../core/utils.js";

const BUILT_IN_PROFILE_ORDER = [
  "passthrough",
  "transcode_1080p60",
  "transcode_720p60",
];

const DEFAULT_PROFILES: TranscodeProfiles = {
  passthrough: {
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
};

const profileTuningRowsExpanded = new Set<string>();

function effectiveTranscodeProfiles(): TranscodeProfiles {
  const current = state.config?.transcodeProfiles;
  if (current && Object.keys(current).length > 0) return current;
  return DEFAULT_PROFILES;
}

function settingsV2Active(): boolean {
  return Boolean(document.getElementById("dashboard-v2-host"));
}

function profileNumber(row: HTMLElement, selector: string, dataKey: keyof HTMLElement["dataset"]): number {
  const input = row.querySelector<HTMLInputElement>(selector);
  const raw = input ? input.value.trim() : row.dataset[dataKey] || "";
  const num = parseInt(raw, 10);
  return Number.isFinite(num) && num >= 0 ? num : 0;
}

function renderProfileTuningFields(p: TranscodeProfile): string {
  const crf = p.crf ?? 23;
  const gop = p.gop ?? 60;
  const bframes = p.bframes ?? 0;
  const bitrate = p.bitrate ?? 0;
  const maxBitrate = p.maxBitrate ?? 0;
  const width = p.width ?? 0;
  const height = p.height ?? 0;

  return `
    <div class="grid grid-cols-2 gap-3 pt-2 sm:grid-cols-4">
      <div>
        <label class="label text-xs">CRF (0=lossless, 23=default)</label>
        <input type="number" min="0" max="51" class="input input-bordered input-sm w-full js-profile-crf" value="${crf}">
      </div>
      <div>
        <label class="label text-xs">GOP (frames)</label>
        <input type="number" min="0" class="input input-bordered input-sm w-full js-profile-gop" value="${gop}">
      </div>
      <div>
        <label class="label text-xs">B-Frames</label>
        <input type="number" min="0" max="16" class="input input-bordered input-sm w-full js-profile-bframes" value="${bframes}">
      </div>
      <div>
        <label class="label text-xs">Bitrate (kbps, 0=auto)</label>
        <input type="number" min="0" class="input input-bordered input-sm w-full js-profile-bitrate" value="${bitrate}">
      </div>
      <div>
        <label class="label text-xs">Max Bitrate (kbps)</label>
        <input type="number" min="0" class="input input-bordered input-sm w-full js-profile-maxbitrate" value="${maxBitrate}">
      </div>
      <div>
        <label class="label text-xs">Width (0=auto)</label>
        <input type="number" min="0" class="input input-bordered input-sm w-full js-profile-width" value="${width}">
      </div>
      <div>
        <label class="label text-xs">Height (0=auto)</label>
        <input type="number" min="0" class="input input-bordered input-sm w-full js-profile-height" value="${height}">
      </div>
    </div>`;
}

function renderProfileRow(name: string, p: TranscodeProfile): string {
  const isBuiltIn = BUILT_IN_PROFILE_ORDER.includes(name);
  const preset = p.preset || "ultrafast";
  const tune = p.tune || "zerolatency";
  const expanded = profileTuningRowsExpanded.has(name);

  return `
    <div class="border-base-content/10 bg-base-200/50 rounded-lg border p-4 space-y-3" data-profile-name="${name}">
      <div class="flex flex-wrap items-center justify-between gap-3">
        <div class="flex items-center gap-2">
          <input type="text" class="input input-bordered input-sm font-mono font-bold js-profile-name" value="${name}" ${isBuiltIn ? "readonly" : ""}>
          ${isBuiltIn ? '<span class="badge badge-ghost badge-sm">Built-in</span>' : ""}
        </div>
        <div class="flex items-center gap-2">
          <button type="button" class="btn btn-ghost btn-xs js-profile-tuning-toggle" data-name="${name}" aria-expanded="${expanded}">
            ${expanded ? "Hide tuning" : "Show tuning"}
          </button>
          ${!isBuiltIn ? '<button type="button" class="btn btn-ghost btn-xs text-error js-profile-delete">Remove</button>' : ""}
        </div>
      </div>
      <div class="grid grid-cols-2 gap-3 sm:grid-cols-2">
        <div>
          <label class="label text-xs">Preset</label>
          <select class="select select-bordered select-sm w-full js-profile-preset">
            <option value="ultrafast" ${preset === "ultrafast" ? "selected" : ""}>ultrafast</option>
            <option value="superfast" ${preset === "superfast" ? "selected" : ""}>superfast</option>
            <option value="veryfast" ${preset === "veryfast" ? "selected" : ""}>veryfast</option>
            <option value="faster" ${preset === "faster" ? "selected" : ""}>faster</option>
            <option value="fast" ${preset === "fast" ? "selected" : ""}>fast</option>
            <option value="medium" ${preset === "medium" ? "selected" : ""}>medium</option>
          </select>
        </div>
        <div>
          <label class="label text-xs">Tune</label>
          <select class="select select-bordered select-sm w-full js-profile-tune">
            <option value="zerolatency" ${tune === "zerolatency" ? "selected" : ""}>zerolatency</option>
            <option value="film" ${tune === "film" ? "selected" : ""}>film</option>
            <option value="animation" ${tune === "animation" ? "selected" : ""}>animation</option>
            <option value="stillimage" ${tune === "stillimage" ? "selected" : ""}>stillimage</option>
          </select>
        </div>
      </div>
      <div data-profile-tuning>${expanded ? renderProfileTuningFields(p) : ""}</div>
    </div>`;
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
        btn.setAttribute(
          "aria-label",
          `${expanded ? "Hide" : "Show"} tuning for ${name}`,
        );
        btn.textContent = expanded ? "Hide tuning" : "Show tuning";
      });
    });
}

function showSavedFeedback(id: string): void {
  const el = document.getElementById(id);
  if (!el) return;
  el.classList.remove("hidden");
  setTimeout(() => el.classList.add("hidden"), 3000);
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
  bindProfileTuningToggles(list);
  list
    .querySelectorAll<HTMLButtonElement>(".js-profile-delete")
    .forEach((btn) => {
      btn.addEventListener("click", () => {
        const row = btn.closest("[data-profile-name]");
        if (row) {
          row.remove();
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
    bindProfileTuningToggles(row);
    row
      .querySelector<HTMLButtonElement>(".js-profile-delete")
      ?.addEventListener("click", () => {
        row.remove();
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
