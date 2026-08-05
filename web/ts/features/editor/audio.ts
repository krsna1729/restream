import {
  isAbsoluteUrl, formatChannelCount, formatCodecName,
} from "../../core/utils.js";
import {
  resolvePresetOutputUrl, detectOutputProtocol, OUTPUT_SERVER_PRESETS,
} from "./output-url.js";
import {
  detectAudioPlatform, detectAudioProtocol, getAudioCaps, getAudioPlatformLabel,
} from "../../core/audio-caps.js";
import type { AudioCaps, AudioProtocol } from "../../core/audio-caps.js";
import { state } from "../../core/state.js";
import type { AudioTrack } from "../../types.js";

export function getDefaultOutputHost(): string {
  return state.config?.ingestHost || "localhost";
}
export function buildSrtUrlFromFields(): string {
  const host =
    (
      document.getElementById("out-srt-host-input") as HTMLInputElement | null
    )?.value.trim() || "";
  const port =
    (
      document.getElementById("out-srt-port-input") as HTMLInputElement | null
    )?.value.trim() || "6000";
  const streamId =
    (
      document.getElementById(
        "out-srt-streamid-input",
      ) as HTMLInputElement | null
    )?.value.trim() || "";
  const extraQueryRaw =
    (
      document.getElementById(
        "out-srt-extra-query-input",
      ) as HTMLInputElement | null
    )?.value.trim() || "";
  const passphrase =
    (
      document.getElementById(
        "out-srt-passphrase-input",
      ) as HTMLInputElement | null
    )?.value.trim() || "";
  const pbkeylen =
    (
      document.getElementById(
        "out-srt-pbkeylen-input",
      ) as HTMLSelectElement | null
    )?.value || "16";

  if (!host) return "";

  const queryParts: string[] = [];
  if (streamId) {
    queryParts.push(`streamid=${streamId}`);
  }
  if (passphrase) {
    queryParts.push(`passphrase=${encodeURIComponent(passphrase)}`);
    queryParts.push(
      `pbkeylen=${pbkeylen === "24" || pbkeylen === "32" ? pbkeylen : "16"}`,
    );
  }
  if (extraQueryRaw) {
    for (const segment of extraQueryRaw.split("&")) {
      const part = segment.trim();
      if (!part) continue;
      queryParts.push(part);
    }
  }

  const qs = queryParts.join("&");
  return `srt://${host}:${port}${qs ? `?${qs}` : ""}`;
}
export function getEffectiveOutputUrlFromModal(): string {
  const protocol =
    (document.getElementById("out-protocol-input") as HTMLSelectElement | null)
      ?.value || "rtmp";
  const serverUrl =
    (
      document.getElementById(
        "out-server-url-input",
      ) as HTMLSelectElement | null
    )?.value || "";
  const rawInput =
    (
      document.getElementById("out-rtmp-key-input") as HTMLInputElement | null
    )?.value.trim() || "";

  if (protocol === "srt") {
    return buildSrtUrlFromFields();
  }

  if (isAbsoluteUrl(rawInput)) {
    return rawInput;
  }

  return resolvePresetOutputUrl(serverUrl, rawInput);
}
export function setOutputToggleBusy(
  button: HTMLButtonElement | null,
  busy: boolean,
): void {
  if (!button) return;
  button.disabled = busy;
  button.classList.toggle("btn-disabled", busy);
}

const pendingOutputToggles = new Set<string>();

function outputToggleKey(pipeId: string, outId: string): string {
  return `${pipeId}:${outId}`;
}

export function isOutputToggleBusy(pipeId: string, outId: string): boolean {
  return pendingOutputToggles.has(outputToggleKey(pipeId, outId));
}

export function setOutputTogglePending(
  pipeId: string,
  outId: string,
  busy: boolean,
): void {
  const key = outputToggleKey(pipeId, outId);
  if (busy) pendingOutputToggles.add(key);
  else pendingOutputToggles.delete(key);
}

type ModalAudioMode = "all" | "subset" | "downmix" | "remap";

export const modalAudioCtx = {
  currentModalAudioTracks: [] as AudioTrack[],
  currentModalIngestLive: false,
  modalAudioMode: "all" as ModalAudioMode,
  modalAudioSelectedTracks: [0] as number[],
};

export function getTrackChannelCount(trackIndex: number): number {
  const track = modalAudioCtx.currentModalAudioTracks[trackIndex];
  return track?.channels || 2;
}

export function populateRemapTrackOptions(
  trackCount: number,
  selectedTrack: number,
): void {
  const trackSelect = document.getElementById(
    "out-remap-track-input",
  ) as HTMLSelectElement | null;
  const trackField = document.getElementById("out-remap-track-field");
  if (!trackSelect || !trackField) return;

  const showTrackSelector = trackCount > 1;
  trackField.classList.toggle("hidden", !showTrackSelector);
  trackField.classList.toggle("inline-block", showTrackSelector);

  trackSelect.innerHTML = Array.from({ length: trackCount }, (_, i) => {
    const ch = modalAudioCtx.currentModalAudioTracks[i]?.channels;
    const label = ch
      ? `Track ${i + 1} (${formatChannelCount(ch)})`
      : `Track ${i + 1}`;
    return `<option value="${i}">${label}</option>`;
  }).join("");
  trackSelect.value = String(Math.min(selectedTrack, trackCount - 1));

  trackSelect.onchange = () => {
    const newTrack = parseInt(trackSelect.value, 10);
    const channelCount = getTrackChannelCount(newTrack);
    populateRemapChannelOptions(channelCount, 0, Math.min(1, channelCount - 1));
  };
}

export function populateRemapChannelOptions(
  channelCount: number,
  selectedLeft: number,
  selectedRight: number,
): void {
  const leftSelect = document.getElementById(
    "out-remap-left-input",
  ) as HTMLSelectElement | null;
  const rightSelect = document.getElementById(
    "out-remap-right-input",
  ) as HTMLSelectElement | null;
  if (!leftSelect || !rightSelect) return;

  const options = Array.from(
    { length: channelCount },
    (_, i) => `<option value="${i}">${i}</option>`,
  ).join("");

  leftSelect.innerHTML = options;
  rightSelect.innerHTML = options;
  leftSelect.value = String(Math.min(selectedLeft, channelCount - 1));
  rightSelect.value = String(Math.min(selectedRight, channelCount - 1));
}

// ── Adaptive audio routing section ─────────────────────

function getModalAudioCapsContext() {
  const url = getEffectiveOutputUrlFromModal();
  const selectProtocol = ((
    document.getElementById("out-protocol-input") as HTMLSelectElement | null
  )?.value || "rtmp") as AudioProtocol;
  const platform = detectAudioPlatform(url);
  const protocol = detectAudioProtocol(url, selectProtocol);
  return { platform, protocol, caps: getAudioCaps(platform, protocol) };
}

function formatTrackPickLabel(trackIndex: number): string {
  const track = modalAudioCtx.currentModalAudioTracks[trackIndex];
  const codec = formatCodecName(track?.codec) || track?.codec || "unknown";
  const channels = track?.channels ? formatChannelCount(track.channels) : "?ch";
  const rate = track?.sample_rate
    ? ` · ${Number.isInteger(track.sample_rate / 1000) ? track.sample_rate / 1000 : (track.sample_rate / 1000).toFixed(1)} kHz`
    : "";
  return `Track ${trackIndex + 1} · ${codec} · ${channels}${rate}`;
}

function getRoutedTrackIndices(mode: ModalAudioMode): number[] {
  if (mode === "all") {
    return Array.from({ length: modalAudioCtx.currentModalAudioTracks.length }, (_, i) => i);
  }
  return modalAudioCtx.modalAudioSelectedTracks;
}

function renderAudioCapsBadges(
  platform: ReturnType<typeof detectAudioPlatform>,
  protocol: AudioProtocol,
  caps: AudioCaps,
): void {
  const capsEl = document.getElementById("out-audio-caps");
  if (!capsEl) return;
  const maxTracks =
    caps.maxTracks === Infinity ? "unlimited" : `${caps.maxTracks} track`;
  const maxChannels =
    caps.maxChannels === Infinity
      ? "unlimited"
      : formatChannelCount(caps.maxChannels);
  const codecs =
    caps.codecs === "any" ? "any" : caps.codecs.join(", ").toUpperCase();
  capsEl.innerHTML = [
    `${getAudioPlatformLabel(platform)} · ${protocol.toUpperCase()}`,
    maxTracks,
    maxChannels,
    `Codecs: ${codecs}`,
  ]
    .map((text) => `<span class="badge badge-sm badge-ghost">${text}</span>`)
    .join("");
}

function renderAudioWarnings(
  platform: ReturnType<typeof detectAudioPlatform>,
  protocol: AudioProtocol,
  caps: AudioCaps,
): void {
  const warningsEl = document.getElementById("out-audio-warnings");
  if (!warningsEl) return;

  const items: { cls: string; text: string }[] = [];
  const platformLabel = getAudioPlatformLabel(platform);
  const protoLabel = protocol.toUpperCase();
  const trackCount = Math.max(1, modalAudioCtx.currentModalAudioTracks.length);
  const routedTracks = getRoutedTrackIndices(modalAudioCtx.modalAudioMode);
  const has51Selected = routedTracks.some((t) => getTrackChannelCount(t) > 2);
  const exceedsCap = routedTracks.some(
    (t) => getTrackChannelCount(t) > caps.maxChannels,
  );

  if (modalAudioCtx.modalAudioMode === "all") {
    items.push({
      cls: "text-base-content/60",
      text:
        trackCount > 1
          ? `Passthrough all ${trackCount} ingest tracks as-is.`
          : "Passthrough the ingest audio track as-is.",
    });
  }
  if (caps.maxTracks === 1 && trackCount > 1 && modalAudioCtx.modalAudioMode !== "remap") {
    items.push({
      cls: "text-warning",
      text: `${platformLabel} ${protoLabel} accepts 1 audio track — the other ${trackCount - 1} ingest track(s) are not sent.`,
    });
  }
  if (modalAudioCtx.modalAudioMode === "downmix" && exceedsCap) {
    items.push({
      cls: "text-warning",
      text: `${platformLabel} supports max ${formatChannelCount(caps.maxChannels)} on ${protoLabel} — the selected track is downmixed to stereo.`,
    });
  }
  if (
    platform === "youtube" &&
    (protocol === "rtmp" || protocol === "rtmps") &&
    (modalAudioCtx.modalAudioMode === "all" || modalAudioCtx.modalAudioMode === "subset") &&
    has51Selected
  ) {
    items.push({
      cls: "text-warning",
      text: `5.1 on YouTube ${protoLabel}: RTMP/RTMPS is stereo only. Use HLS for 5.1 surround.`,
    });
  }
  if (
    platform === "youtube" &&
    protocol === "hls" &&
    (modalAudioCtx.modalAudioMode === "all" || modalAudioCtx.modalAudioMode === "subset") &&
    has51Selected
  ) {
    items.push({
      cls: "text-success",
      text: "5.1 pass-through supported on YouTube HLS (AAC / AC3 / EAC3).",
    });
  }
  if (platform === "facebook" && modalAudioCtx.modalAudioMode !== "all") {
    items.push({
      cls: "text-base-content/60",
      text: "AAC-LC stereo, 44.1/48 kHz, 128 kbps recommended (256 max).",
    });
  }
  if (platform === "vdocipher" && modalAudioCtx.modalAudioMode !== "all") {
    items.push({
      cls: "text-base-content/60",
      text: "Multi-track or surround audio will be downmixed or fail.",
    });
  }
  if (
    platform === "generic" &&
    (protocol === "srt" || protocol === "hls") &&
    (modalAudioCtx.modalAudioMode === "all" || modalAudioCtx.modalAudioMode === "subset") &&
    routedTracks.length > 1
  ) {
    items.push({
      cls: "text-success",
      text:
        modalAudioCtx.modalAudioMode === "all"
          ? `${protoLabel} supports multi-track — all ${routedTracks.length} ingest tracks are sent.`
          : `${protoLabel} supports multi-track — all ${routedTracks.length} selected tracks are sent.`,
    });
  }

  warningsEl.innerHTML = items
    .filter((item) => item.text)
    .map((item) => `<p class="${item.cls} text-xs">${item.text}</p>`)
    .join("");
}

function renderAudioTrackPicker(multiSelect: boolean): void {
  const pickEl = document.getElementById("out-audio-track-pick");
  if (!pickEl) return;

  const trackCount = Math.max(1, modalAudioCtx.currentModalAudioTracks.length);
  pickEl.innerHTML = Array.from({ length: trackCount }, (_, i) => {
    const checked = modalAudioCtx.modalAudioSelectedTracks.includes(i) ? " checked" : "";
    const type = multiSelect ? "checkbox" : "radio";
    const klass = multiSelect ? "checkbox checkbox-sm" : "radio radio-sm";
    return `<label class="border-base-content/10 bg-base-100 hover:bg-base-100/80 flex min-w-0 cursor-pointer items-start gap-3 rounded-lg border px-3 py-2 text-sm">
            <input type="${type}" name="out-audio-track" value="${i}" class="${klass}"${checked} />
            <span class="min-w-0 leading-5">${formatTrackPickLabel(i)}</span>
        </label>`;
  }).join("");

  pickEl.querySelectorAll('input[name="out-audio-track"]').forEach((input) => {
    (input as HTMLInputElement).onchange = () => {
      const checkedValues = Array.from(
        pickEl.querySelectorAll('input[name="out-audio-track"]:checked'),
      ).map((el) => parseInt((el as HTMLInputElement).value, 10));
      if (checkedValues.length === 0) {
        refreshAudioRoutingUi();
        return;
      }
      modalAudioCtx.modalAudioSelectedTracks = checkedValues.sort((a, b) => a - b);
      refreshAudioRoutingUi();
    };
  });
}

export function refreshAudioRoutingUi(): void {
  const section = document.getElementById("out-audio-section");
  if (!section) return;

  const encoding =
    (document.getElementById("out-encoding-input") as HTMLSelectElement | null)
      ?.value || "source";
  // Audio routing is always enabled — any video encoding can be combined with
  // audio routing via the compound format (e.g. "720p+atrack:0,1").
  const routingEnabled = true;
  const { platform, protocol, caps } = getModalAudioCapsContext();

  renderAudioCapsBadges(platform, protocol, caps);

  const ingestEl = document.getElementById("out-audio-ingest");
  if (ingestEl) {
    const trackCount = modalAudioCtx.currentModalAudioTracks.length;
    ingestEl.textContent = modalAudioCtx.currentModalIngestLive
      ? `Detected ingest: ${trackCount} audio track(s) — ` +
        modalAudioCtx.currentModalAudioTracks
          .map(
            (t, i) =>
              `Track ${i + 1}: ${formatCodecName(t.codec) || t.codec || "?"} ${t.channels ? formatChannelCount(t.channels) : "?ch"}`,
          )
          .join(", ")
      : "No active ingest — track list unavailable; defaults to Track 1.";
  }

  document
    .getElementById("out-audio-encoding-note")
    ?.classList.toggle("hidden", routingEnabled);
  document
    .getElementById("out-audio-controls")
    ?.classList.toggle("hidden", !routingEnabled);

  const warningsEl = document.getElementById("out-audio-warnings");
  if (!routingEnabled) {
    if (warningsEl) warningsEl.innerHTML = "";
    return;
  }

  const trackCount = Math.max(1, modalAudioCtx.currentModalAudioTracks.length);
  modalAudioCtx.modalAudioSelectedTracks = modalAudioCtx.modalAudioSelectedTracks.filter(
    (t) => t < trackCount,
  );
  if (modalAudioCtx.modalAudioSelectedTracks.length === 0) modalAudioCtx.modalAudioSelectedTracks = [0];

  const multiAllowed = caps.maxTracks > 1;
  if (!multiAllowed || modalAudioCtx.modalAudioMode !== "subset") {
    modalAudioCtx.modalAudioSelectedTracks = [modalAudioCtx.modalAudioSelectedTracks[0]];
  }

  const passBlocked = modalAudioCtx.modalAudioSelectedTracks.some(
    (t) => getTrackChannelCount(t) > caps.maxChannels,
  );
  if (modalAudioCtx.modalAudioMode === "subset" && passBlocked) {
    modalAudioCtx.modalAudioMode = "downmix";
  }

  document.querySelectorAll("#out-audio-mode [data-amode]").forEach((el) => {
    const button = el as HTMLButtonElement;
    const mode = button.dataset.amode as ModalAudioMode;
    button.classList.toggle("btn-active", mode === modalAudioCtx.modalAudioMode);
    const disabled = mode === "subset" && passBlocked;
    button.disabled = disabled;
    button.title = disabled
      ? "Selected track exceeds the destination channel limit — downmix required."
      : "";
    button.onclick = () => {
      modalAudioCtx.modalAudioMode = mode;
      refreshAudioRoutingUi();
    };
  });

  const showPicker =
    modalAudioCtx.modalAudioMode === "subset" || modalAudioCtx.modalAudioMode === "downmix";
  document
    .getElementById("out-audio-track-pick")
    ?.classList.toggle("hidden", !showPicker);
  if (showPicker) {
    renderAudioTrackPicker(modalAudioCtx.modalAudioMode === "subset" && multiAllowed);
  }

  const remapFields = document.getElementById("out-remap-fields");
  if (remapFields) {
    remapFields.classList.toggle("hidden", modalAudioCtx.modalAudioMode !== "remap");
    remapFields.classList.toggle("flex", modalAudioCtx.modalAudioMode === "remap");
  }

  renderAudioWarnings(platform, protocol, caps);
}
