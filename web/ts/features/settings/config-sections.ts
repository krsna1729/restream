import { patchConfig } from "../../core/api.js";
import { state } from "../../core/state.js";

function showSavedFeedback(id: string): void {
  const el = document.getElementById(id);
  if (!el) return;
  el.classList.remove("hidden");
  setTimeout(() => el.classList.add("hidden"), 3000);
}

export async function saveServerName(): Promise<void> {
  const input = document.getElementById("settings-server-name") as HTMLInputElement | null;
  if (!input) return;
  const serverName = input.value.trim();
  const res = await patchConfig({ serverName });
  if (res) {
    state.config = { ...state.config, serverName: res.serverName };
    showSavedFeedback("server-name-saved");
  }
}

export async function saveIngestHost(): Promise<void> {
  const input = document.getElementById("settings-ingest-host") as HTMLInputElement | null;
  if (!input) return;
  const ingestHost = input.value.trim();
  const res = await patchConfig({ ingestHost });
  if (res) {
    state.config = { ...state.config, ingestHost: res.ingestHost };
    showSavedFeedback("ingest-host-saved");
  }
}

export function populateRecordingSettings(): void {
  const enabledInput = document.getElementById("settings-rec-enabled") as HTMLInputElement | null;
  const dirInput = document.getElementById("settings-rec-dir") as HTMLInputElement | null;
  const formatInput = document.getElementById("settings-rec-format") as HTMLSelectElement | null;
  const patternInput = document.getElementById("settings-rec-pattern") as HTMLInputElement | null;
  const segmentInput = document.getElementById("settings-rec-segment") as HTMLInputElement | null;

  const rec = (state.config as any)?.recordingSettings;
  if (enabledInput) enabledInput.checked = rec?.enabled ?? false;
  if (dirInput) dirInput.value = rec?.outputDir ?? "./recordings";
  if (formatInput) formatInput.value = rec?.format ?? "flv";
  if (patternInput) patternInput.value = rec?.filenamePattern ?? "{stream_key}_{timestamp}";
  if (segmentInput) segmentInput.value = String(rec?.segmentDurationSecs ?? 0);
}

export async function saveRecordingSettings(): Promise<void> {
  const enabledInput = document.getElementById("settings-rec-enabled") as HTMLInputElement | null;
  const dirInput = document.getElementById("settings-rec-dir") as HTMLInputElement | null;
  const formatInput = document.getElementById("settings-rec-format") as HTMLSelectElement | null;
  const patternInput = document.getElementById("settings-rec-pattern") as HTMLInputElement | null;
  const segmentInput = document.getElementById("settings-rec-segment") as HTMLInputElement | null;

  const recordingSettings = {
    enabled: enabledInput?.checked ?? false,
    outputDir: dirInput?.value.trim() || "./recordings",
    format: formatInput?.value || "flv",
    filenamePattern: patternInput?.value.trim() || "{stream_key}_{timestamp}",
    segmentDurationSecs: parseInt(segmentInput?.value || "0", 10) || 0,
  };

  const res = await patchConfig({ recordingSettings: recordingSettings as any });
  if (res) {
    state.config = { ...state.config, ...res };
    showSavedFeedback("recording-settings-saved");
  }
}

export function populateSrtIngestSettings(): void {
  const enabledInput = document.getElementById("settings-srt-enabled") as HTMLInputElement | null;
  const portInput = document.getElementById("settings-srt-port") as HTMLInputElement | null;
  const latencyInput = document.getElementById("settings-srt-latency") as HTMLInputElement | null;
  const passphraseInput = document.getElementById("settings-srt-passphrase") as HTMLInputElement | null;

  const srt = (state.config as any)?.srtIngest;
  if (enabledInput) enabledInput.checked = srt?.enabled ?? false;
  if (portInput) portInput.value = String(srt?.port ?? 6000);
  if (latencyInput) latencyInput.value = String(srt?.latencyMs ?? 120);
  if (passphraseInput) passphraseInput.value = srt?.passphrase ?? "";
}

export async function saveSrtIngest(): Promise<void> {
  const enabledInput = document.getElementById("settings-srt-enabled") as HTMLInputElement | null;
  const portInput = document.getElementById("settings-srt-port") as HTMLInputElement | null;
  const latencyInput = document.getElementById("settings-srt-latency") as HTMLInputElement | null;
  const passphraseInput = document.getElementById("settings-srt-passphrase") as HTMLInputElement | null;

  const srtIngest = {
    enabled: enabledInput?.checked ?? false,
    port: parseInt(portInput?.value || "6000", 10) || 6000,
    latencyMs: parseInt(latencyInput?.value || "120", 10) || 120,
    passphrase: passphraseInput?.value || "",
  };

  const res = await patchConfig({ srtIngest: srtIngest as any });
  if (res) {
    state.config = { ...state.config, ...res };
    showSavedFeedback("srt-ingest-saved");
  }
}

export function populateBackendPolicySettings(): void {
  const allowExecInput = document.getElementById("settings-backend-allow-exec") as HTMLInputElement | null;
  const preferredEngineInput = document.getElementById("settings-backend-preferred-engine") as HTMLSelectElement | null;
  const strictModeInput = document.getElementById("settings-backend-strict-mode") as HTMLInputElement | null;

  const policy = (state.config as any)?.backendPolicy;
  if (allowExecInput) allowExecInput.checked = policy?.allowExternalTranscoderExec ?? true;
  if (preferredEngineInput) preferredEngineInput.value = policy?.preferredEngine ?? "auto";
  if (strictModeInput) strictModeInput.checked = policy?.strictMode ?? false;
}

export async function saveBackendPolicy(): Promise<void> {
  const allowExecInput = document.getElementById("settings-backend-allow-exec") as HTMLInputElement | null;
  const preferredEngineInput = document.getElementById("settings-backend-preferred-engine") as HTMLSelectElement | null;
  const strictModeInput = document.getElementById("settings-backend-strict-mode") as HTMLInputElement | null;

  const backendPolicy = {
    allowExternalTranscoderExec: allowExecInput?.checked ?? true,
    preferredEngine: (preferredEngineInput?.value as "auto" | "builtin" | "ffmpeg") || "auto",
    strictMode: strictModeInput?.checked ?? false,
  };

  const res = await patchConfig({ backendPolicy: backendPolicy as any });
  if (res) {
    state.config = { ...state.config, ...res };
    showSavedFeedback("backend-policy-saved");
  }
}
