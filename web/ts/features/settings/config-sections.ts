import { patchConfig } from "../../core/api.js";
import { state } from "../../core/state.js";
import type { BackendPolicy, RecordingSettings, SrtGlobalIngestConfig } from "../../types.js";

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
  const retainSourceTsInput = document.getElementById(
    "recording-retain-source-ts",
  ) as HTMLInputElement | null;

  const rec = state.config?.recordingSettings;
  if (retainSourceTsInput) retainSourceTsInput.checked = rec?.retainSourceTs ?? false;
}

export async function saveRecordingSettings(): Promise<void> {
  const retainSourceTsInput = document.getElementById(
    "recording-retain-source-ts",
  ) as HTMLInputElement | null;

  const recordingSettings: RecordingSettings = {
    retainSourceTs: retainSourceTsInput?.checked ?? false,
  };

  const res = await patchConfig({ recordingSettings });
  if (res) {
    state.config = { ...state.config, ...res };
    showSavedFeedback("recording-settings-saved");
  }
}

function parseSrtIngestPbkeylen(value: string | undefined): 16 | 24 | 32 {
  const parsed = parseInt(value || "16", 10);
  return parsed === 24 || parsed === 32 ? parsed : 16;
}

export function populateSrtIngestSettings(): void {
  const modeInput = document.getElementById("srt-ingest-mode-input") as HTMLSelectElement | null;
  const passphraseInput = document.getElementById(
    "srt-ingest-passphrase-input",
  ) as HTMLInputElement | null;
  const pbkeylenInput = document.getElementById(
    "srt-ingest-pbkeylen-input",
  ) as HTMLSelectElement | null;
  const latencyInput = document.getElementById(
    "srt-ingest-latency-ms-input",
  ) as HTMLInputElement | null;

  const srt = state.config?.srtIngest;
  if (modeInput) modeInput.value = srt?.mode ?? "plaintext";
  if (passphraseInput) passphraseInput.value = srt?.passphrase ?? "";
  if (pbkeylenInput) pbkeylenInput.value = String(srt?.pbkeylen ?? 16);
  if (latencyInput) latencyInput.value = String(srt?.latencyMs ?? 250);
}

export async function saveSrtIngest(): Promise<void> {
  const modeInput = document.getElementById("srt-ingest-mode-input") as HTMLSelectElement | null;
  const passphraseInput = document.getElementById(
    "srt-ingest-passphrase-input",
  ) as HTMLInputElement | null;
  const pbkeylenInput = document.getElementById(
    "srt-ingest-pbkeylen-input",
  ) as HTMLSelectElement | null;
  const latencyInput = document.getElementById(
    "srt-ingest-latency-ms-input",
  ) as HTMLInputElement | null;

  const mode = modeInput?.value === "encrypted" ? "encrypted" : "plaintext";
  const srtIngest: SrtGlobalIngestConfig = {
    mode,
    passphrase: mode === "encrypted" ? passphraseInput?.value || "" : null,
    pbkeylen: parseSrtIngestPbkeylen(pbkeylenInput?.value),
    latencyMs: parseInt(latencyInput?.value || "250", 10) || 250,
  };

  const res = await patchConfig({ srtIngest });
  if (res) {
    state.config = { ...state.config, ...res };
    showSavedFeedback("srt-ingest-saved");
  }
}

export function populateBackendPolicySettings(): void {
  const videoPresetsInput = document.getElementById(
    "backend-policy-internal-video-presets",
  ) as HTMLInputElement | null;
  const hevcToH264Input = document.getElementById(
    "backend-policy-internal-hevc-to-h264",
  ) as HTMLInputElement | null;
  const hlsPreviewInput = document.getElementById(
    "backend-policy-internal-hls-preview",
  ) as HTMLInputElement | null;
  const complexAudioInput = document.getElementById(
    "backend-policy-internal-complex-audio",
  ) as HTMLInputElement | null;

  const policy = state.config?.backendPolicy;
  if (videoPresetsInput) videoPresetsInput.checked = policy?.internalVideoPresets ?? false;
  if (hevcToH264Input) hevcToH264Input.checked = policy?.internalHevcToH264 ?? false;
  if (hlsPreviewInput) hlsPreviewInput.checked = policy?.internalHlsPreview ?? false;
  if (complexAudioInput) complexAudioInput.checked = policy?.internalComplexAudio ?? false;
}

export async function saveBackendPolicy(): Promise<void> {
  const videoPresetsInput = document.getElementById(
    "backend-policy-internal-video-presets",
  ) as HTMLInputElement | null;
  const hevcToH264Input = document.getElementById(
    "backend-policy-internal-hevc-to-h264",
  ) as HTMLInputElement | null;
  const hlsPreviewInput = document.getElementById(
    "backend-policy-internal-hls-preview",
  ) as HTMLInputElement | null;
  const complexAudioInput = document.getElementById(
    "backend-policy-internal-complex-audio",
  ) as HTMLInputElement | null;

  const backendPolicy: BackendPolicy = {
    internalVideoPresets: videoPresetsInput?.checked ?? false,
    internalHevcToH264: hevcToH264Input?.checked ?? false,
    internalHlsPreview: hlsPreviewInput?.checked ?? false,
    internalComplexAudio: complexAudioInput?.checked ?? false,
  };

  const res = await patchConfig({ backendPolicy });
  if (res) {
    state.config = { ...state.config, ...res };
    showSavedFeedback("backend-policy-saved");
  }
}
