import {
  isOutputToggleBusy, getDefaultOutputHost, getEffectiveOutputUrlFromModal,
  getTrackChannelCount, populateRemapTrackOptions, populateRemapChannelOptions,
  refreshAudioRoutingUi,
  setOutputTogglePending, setOutputToggleBusy,
  modalAudioCtx,
} from "./audio.js";
import {
  pipeFormBtn, addPipeBtn, editPipeBtn, deletePipeBtn,
  loadStreamKeysOnce, getSuggestedPipelineName,
  populateOutputEncodingSelect,
} from "./pipeline.js";
import {
  getStreamKeys,
  startOut,
  stopOut,
  createPipeline,
  updatePipeline,
  deletePipeline,
  createOutput,
  updateOutput,
  deleteOutput,
  listMediaFiles,
  getPipelineFileIngest,
  getMediaFileAnalysis,
} from "../../core/api.js";
import type {
  MediaFile,
  MediaFileAnalysis,
  PipelineFileIngestConfig,
} from "../../core/api.js";
import {
  getUrlParam,
  isValidOutput,
  isValidMonitoringUrl,
  setUrlParam,
  isAbsoluteUrl,
  protocolUsesOutputServerPresets,
  resolvePresetOutputUrl,
  matchOutputServerPreset,
  detectOutputProtocol,
  extractCandidateStreamToken,
  getDefaultOutputToken,
  parseSrtFields,
  buildDefaultCustomOutputUrl,
  formatMaskedStreamKey,
  formatChannelCount,
  formatCodecName,
  escapeHtml,
  showErrorAlert,
  confirmInApp,
  OUTPUT_SERVER_PRESETS,
} from "../../core/utils.js";
import type { MatchedPreset, SrtFields } from "../../core/utils.js";
import {
  detectAudioPlatform,
  detectAudioProtocol,
  getAudioCaps,
  getAudioPlatformLabel,
} from "../../core/audio-caps.js";
import type { AudioCaps, AudioProtocol } from "../../core/audio-caps.js";
import { isOutputManagedActive } from "../../core/output-status.js";
import {
  normalizeOutputConfig,
  outputConfigRtmpMode,
} from "../../core/output-config.js";
import { state } from "../../core/state.js";
import {
  awaitDashboardRuntimeMutationConvergence,
  removeDashboardOutputConfig,
  removeDashboardPipelineConfig,
  refreshDashboardRuntime,
  upsertDashboardPipelineConfig,
  upsertDashboardOutputConfig,
} from "../dashboard.js";
import {
  beginOutputControlIntent,
  finishOutputControlIntent,
  setOutputControlError,
} from "../output-control-state.js";
import type {
  AudioTrack,
  ConfigPipeline,
  OutputConfig,
  OutputVideoCodec,
  PipelineView,
  OutputView,
  RtmpOutputMode,
  SrtPipelineIngestConfig,
  StreamKey,
} from "../../types.js";

export { isOutputToggleBusy } from "./audio.js";
export { addPipeBtn, deletePipeBtn, editPipeBtn, pipeFormBtn } from "./pipeline.js";

function currentOutputView(pipeId: string, outId: string): OutputView | null {
  return (
    state.pipelines
      .find((pipe) => pipe.id === pipeId)
      ?.outs.find((out) => out.id === outId) || null
  );
}

function outputControlConverged(
  pipeId: string,
  outId: string,
  intent: "starting" | "stopping",
): boolean {
  const output = currentOutputView(pipeId, outId);
  if (!output) return false;
  if (intent === "stopping") {
    return output.desiredState === "stopped" && !isOutputManagedActive(output);
  }
  return output.desiredState !== "stopped" && output.status !== "off";
}

function populateOutputServerOptions(
  protocol: string,
  selectedValue = "",
): void {
  const serverSelect = document.getElementById(
    "out-server-url-input",
  ) as HTMLSelectElement | null;
  if (!serverSelect) return;

  const presets = OUTPUT_SERVER_PRESETS[protocol] || OUTPUT_SERVER_PRESETS.rtmp;
  serverSelect.innerHTML = presets
    .map((p) => `<option value="${p.value}">${p.label}</option>`)
    .join("");
  serverSelect.value = presets.some((p) => p.value === selectedValue)
    ? selectedValue
    : "";
}


function isCustomOutputServerSelected(protocol = "rtmp"): boolean {
  const serverSelect = document.getElementById(
    "out-server-url-input",
  ) as HTMLSelectElement | null;
  if (!protocolUsesOutputServerPresets(protocol)) return true;
  return !serverSelect || !serverSelect.value;
}

function selectedCustomRtmpMode(): RtmpOutputMode {
  const value =
    (
      document.getElementById("out-rtmp-mode-input") as HTMLSelectElement | null
    )?.value || "legacy";
  return value === "enhanced" ? "enhanced" : "legacy";
}

function resolveModalRtmpMode(
  protocol: string,
  serverUrl: string,
): RtmpOutputMode {
  if (protocol !== "rtmp") return "legacy";
  const preset = (OUTPUT_SERVER_PRESETS.rtmp || []).find(
    (candidate) => candidate.value === serverUrl,
  );
  return preset?.rtmpMode || selectedCustomRtmpMode();
}

function refreshRtmpModeUi(protocol: string): void {
  const field = document.getElementById("out-rtmp-mode-field");
  const input = document.getElementById(
    "out-rtmp-mode-input",
  ) as HTMLSelectElement | null;
  const show = protocol === "rtmp" && isCustomOutputServerSelected(protocol);
  field?.classList.toggle("hidden", !show);
  if (input) {
    input.disabled = !show;
  }
}

function applyOutputProtocolUi(protocol: string): void {
  const urlLabel = document.getElementById("out-url-input-label");
  const urlField = document.getElementById("out-url-field");
  const serverField = document.getElementById("out-server-url-field");
  const serverSelect = document.getElementById(
    "out-server-url-input",
  ) as HTMLSelectElement | null;
  const srtFields = document.getElementById("out-srt-fields");

  const isPresetBackedMode =
    protocolUsesOutputServerPresets(protocol) &&
    !isCustomOutputServerSelected(protocol);
  const showPresetFields = protocolUsesOutputServerPresets(protocol);
  const showUrlField = protocol !== "srt";

  if (urlLabel) {
    urlLabel.textContent = isPresetBackedMode ? "Stream Key" : "Custom URL";
  }
  if (urlField) {
    urlField.classList.toggle("hidden", !showUrlField);
  }
  if (serverField) {
    serverField.classList.toggle("hidden", !showPresetFields);
  }
  if (srtFields) {
    srtFields.classList.toggle("hidden", protocol !== "srt");
  }
  if (serverSelect) {
    serverSelect.disabled = !showPresetFields;
  }
  refreshRtmpModeUi(protocol);
}


function setupOutputModalProtocolHandlers(): void {
  const protocolSelect = document.getElementById(
    "out-protocol-input",
  ) as HTMLSelectElement | null;
  const serverSelect = document.getElementById(
    "out-server-url-input",
  ) as HTMLSelectElement | null;
  const rawInput = document.getElementById(
    "out-rtmp-key-input",
  ) as HTMLInputElement | null;

  if (!protocolSelect || !serverSelect || !rawInput) return;

  protocolSelect.onchange = () => {
    const protocol = protocolSelect.value || "rtmp";
    const previousRaw = rawInput.value.trim();

    if (protocol === "rtmp") {
      const matchedPreset = matchOutputServerPreset("rtmp", previousRaw);
      const selectedServer = matchedPreset?.value || "";
      populateOutputServerOptions("rtmp", selectedServer);
      rawInput.value = matchedPreset
        ? matchedPreset.inputValue
        : isAbsoluteUrl(previousRaw)
          ? previousRaw
          : buildDefaultCustomOutputUrl(
              "rtmp",
              previousRaw,
              getDefaultOutputHost(),
            );
      applyOutputProtocolUi("rtmp");
      return;
    }

    if (protocol === "hls") {
      const matchedPreset =
        detectOutputProtocol(previousRaw) === "hls"
          ? matchOutputServerPreset("hls", previousRaw)
          : null;
      const selectedServer =
        matchedPreset?.value || OUTPUT_SERVER_PRESETS.hls[0]?.value || "";

      populateOutputServerOptions("hls", selectedServer);
      rawInput.value =
        matchedPreset?.inputValue ||
        extractCandidateStreamToken(previousRaw) ||
        getDefaultOutputToken(previousRaw);
      applyOutputProtocolUi("hls");
      return;
    }

    populateOutputServerOptions("rtmp", "");
    applyOutputProtocolUi(protocol);

    if (protocol === "srt") {
      const values = parseSrtFields(previousRaw, getDefaultOutputHost());
      (
        document.getElementById("out-srt-host-input") as HTMLInputElement
      ).value = values.host;
      (
        document.getElementById("out-srt-port-input") as HTMLInputElement
      ).value = values.port;
      (
        document.getElementById("out-srt-streamid-input") as HTMLInputElement
      ).value = values.streamId;
      (
        document.getElementById("out-srt-passphrase-input") as HTMLInputElement
      ).value = values.passphrase;
      (
        document.getElementById("out-srt-pbkeylen-input") as HTMLSelectElement
      ).value =
        values.pbkeylen === "24" || values.pbkeylen === "32"
          ? values.pbkeylen
          : "16";
      (
        document.getElementById("out-srt-extra-query-input") as HTMLInputElement
      ).value = values.extraQuery;
    }
  };

  serverSelect.onchange = () => {
    const protocol = protocolSelect.value || "rtmp";
    if (protocol === "rtmp" || protocol === "hls") {
      const rawValue = rawInput.value.trim();
      if (serverSelect.value) {
        rawInput.value =
          extractCandidateStreamToken(rawValue) ||
          getDefaultOutputToken(rawValue);
      } else {
        rawInput.value = isAbsoluteUrl(rawValue)
          ? rawValue
          : buildDefaultCustomOutputUrl(
              protocol,
              rawValue,
              getDefaultOutputHost(),
            );
      }
      applyOutputProtocolUi(protocol);
    }
  };

  // Re-evaluate audio caps whenever the destination (platform/protocol) changes.
  const chainAudioRefresh = (
    el: HTMLElement & { onchange?: unknown; oninput?: unknown },
    prop: "onchange" | "oninput",
  ) => {
    const prev = el[prop] as ((ev: Event) => void) | null;
    (el as unknown as Record<string, unknown>)[prop] = (ev: Event) => {
      prev?.(ev);
      refreshAudioRoutingUi();
    };
  };

  rawInput.oninput = () => {
    const rawValue = rawInput.value.trim();
    const currentProtocol = protocolSelect.value || "rtmp";
    const detectedProtocol = isAbsoluteUrl(rawValue)
      ? detectOutputProtocol(rawValue)
      : null;
    if (detectedProtocol && detectedProtocol !== currentProtocol) {
      protocolSelect.value = detectedProtocol;
      populateOutputServerOptions(detectedProtocol, "");
      applyOutputProtocolUi(detectedProtocol);
    }

    const protocol = protocolSelect.value || "rtmp";
    if (protocol === "rtmp" || protocol === "hls") {
      if (!isCustomOutputServerSelected(protocol) && isAbsoluteUrl(rawValue)) {
        const matchedPreset = matchOutputServerPreset(protocol, rawValue);
        if (matchedPreset) {
          serverSelect.value = matchedPreset.value;
          rawInput.value = matchedPreset.inputValue;
        } else if (serverSelect.value) {
          serverSelect.value = "";
        }
      }

      applyOutputProtocolUi(protocol);
    }
  };

  chainAudioRefresh(protocolSelect, "onchange");
  chainAudioRefresh(serverSelect, "onchange");
  chainAudioRefresh(rawInput, "oninput");

  // SRT host/port changes can switch the effective destination.
  for (const id of ["out-srt-host-input", "out-srt-port-input"]) {
    const srtInput = document.getElementById(id) as HTMLInputElement | null;
    if (srtInput) srtInput.oninput = () => refreshAudioRoutingUi();
  }
}


export function onOutEncodingChange(_encoding: string): void {
  refreshAudioRoutingUi();
}

export async function startOutBtn(
  pipeId: string,
  outId: string,
  button: HTMLButtonElement | null = null,
): Promise<void> {
  if (isOutputToggleBusy(pipeId, outId)) return;
  setOutputTogglePending(pipeId, outId, true);
  beginOutputControlIntent(pipeId, outId, "starting");
  setOutputToggleBusy(button, true);
  try {
    const res = await startOut(pipeId, outId);
    if (res !== null) {
      if (res.output?.id && res.output?.pipelineId) {
        upsertDashboardOutputConfig(res.output);
      }
      await awaitDashboardRuntimeMutationConvergence(() =>
        outputControlConverged(pipeId, outId, "starting"),
      );
    } else {
      setOutputControlError(
        pipeId,
        outId,
        "Start output did not complete. Check the error banner and retry when ready.",
      );
    }
  } finally {
    finishOutputControlIntent(pipeId, outId);
    setOutputTogglePending(pipeId, outId, false);
    setOutputToggleBusy(button, false);
  }
}

export async function stopOutBtn(
  pipeId: string,
  outId: string,
  button: HTMLButtonElement | null = null,
): Promise<void> {
  if (isOutputToggleBusy(pipeId, outId)) return;
  setOutputTogglePending(pipeId, outId, true);
  beginOutputControlIntent(pipeId, outId, "stopping");
  setOutputToggleBusy(button, true);
  try {
    const res = await stopOut(pipeId, outId);
    if (res !== null) {
      if (res.output?.id && res.output?.pipelineId) {
        upsertDashboardOutputConfig(res.output);
      }
      await awaitDashboardRuntimeMutationConvergence(() =>
        outputControlConverged(pipeId, outId, "stopping"),
      );
    } else {
      setOutputControlError(
        pipeId,
        outId,
        "Stop output did not complete. Check the error banner and retry when ready.",
      );
    }
  } finally {
    finishOutputControlIntent(pipeId, outId);
    setOutputTogglePending(pipeId, outId, false);
    setOutputToggleBusy(button, false);
  }
}


async function openOutModal(
  mode: "edit" | "create",
  pipe: PipelineView,
  output: OutputView | null = null,
): Promise<void> {
  (document.getElementById("out-mode-input") as HTMLInputElement).value = mode;
  (document.getElementById("out-pipe-id-input") as HTMLInputElement).value =
    pipe.id;
  (document.getElementById("out-id-input") as HTMLInputElement).value =
    output?.id || "";
  const outModalTitle = document.getElementById("out-modal-title");
  if (outModalTitle) {
    outModalTitle.innerText =
      mode === "edit"
        ? `Edit Output "${output?.name || pipe.name}"`
        : `Add Output for "${pipe.name}"`;
  }
  const outSubmitBtn = document.getElementById(
    "out-submit-btn",
  ) as HTMLButtonElement | null;
  if (outSubmitBtn)
    outSubmitBtn.innerText = mode === "edit" ? "Update" : "Create";
  (document.getElementById("out-name-input") as HTMLInputElement).value =
    output?.name || `Out_${pipe.outs.length + 1}`;

  const outputConfig = output
    ? normalizeOutputConfig(output)
    : ({
        video: { mode: "source", codec: "auto" },
        audio: { mode: "all" },
      } as OutputConfig);
  let remapTrack =
    outputConfig.audio.mode === "remap" ? outputConfig.audio.track || 0 : 0;
  let remapLeft =
    outputConfig.audio.mode === "remap" ? outputConfig.audio.leftChannel : 0;
  let remapRight =
    outputConfig.audio.mode === "remap" ? outputConfig.audio.rightChannel : 1;
  modalAudioCtx.currentModalAudioTracks = pipe.input.audioTracks || [];
  if (modalAudioCtx.currentModalAudioTracks.length === 0 && pipe.input.audio) {
    modalAudioCtx.currentModalAudioTracks = [pipe.input.audio];
  }
  modalAudioCtx.currentModalIngestLive = pipe.input.status === "on";

  modalAudioCtx.modalAudioMode =
    outputConfig.audio.mode === "remap"
      ? "remap"
      : outputConfig.audio.mode === "selectTracks"
        ? "subset"
        : outputConfig.audio.mode === "downmix"
          ? "downmix"
          : "all";
  modalAudioCtx.modalAudioSelectedTracks =
    outputConfig.audio.mode === "selectTracks"
      ? outputConfig.audio.tracks
      : outputConfig.audio.mode === "downmix"
        ? [outputConfig.audio.track]
        : [0];

  populateOutputEncodingSelect(
    outputConfig.video.mode === "preset" ? outputConfig.video.preset : "source",
  );
  const codecInput = document.getElementById("out-video-codec-input") as HTMLSelectElement | null;
  if (codecInput) codecInput.value = outputConfig.video.mode === "custom" ? "auto" : outputConfig.video.codec || "auto";
  const trackCount = Math.max(1, modalAudioCtx.currentModalAudioTracks.length);
  populateRemapTrackOptions(trackCount, remapTrack);
  populateRemapChannelOptions(
    getTrackChannelCount(remapTrack),
    remapLeft,
    remapRight,
  );

  const isRunning =
    mode === "edit" && !!output && isOutputManagedActive(output);

  const monitoringUrlInput = document.getElementById(
    "out-monitoring-url-input",
  ) as HTMLInputElement | null;
  if (monitoringUrlInput)
    monitoringUrlInput.value = output?.monitoringUrl || "";
  document
    .getElementById("out-monitoring-url-input")
    ?.classList.remove("input-error");
  document.getElementById("out-monitoring-error")?.classList.add("hidden");
  document
    .getElementById("out-running-lock-note")
    ?.classList.toggle("hidden", !isRunning);

  const baseRtmpUrl = `rtmp://${getDefaultOutputHost()}:1935/live/`;
  const isCreateMode = mode !== "edit" || !output;
  const currentUrl = isCreateMode
    ? `${baseRtmpUrl}test`
    : output?.url || `${baseRtmpUrl}test`;
  const detectedProtocol = detectOutputProtocol(currentUrl);
  const protocolSelect = document.getElementById(
    "out-protocol-input",
  ) as HTMLSelectElement | null;
  const serverSelect = document.getElementById(
    "out-server-url-input",
  ) as HTMLSelectElement | null;
  const matchedPreset = protocolUsesOutputServerPresets(detectedProtocol)
    ? matchOutputServerPreset(detectedProtocol, currentUrl)
    : null;
  if (protocolSelect) {
    protocolSelect.value = detectedProtocol;
  }
  populateOutputServerOptions(detectedProtocol, matchedPreset?.value || "");

  if (serverSelect) {
    serverSelect.value = matchedPreset?.value || "";
  }
  const rtmpModeInput = document.getElementById(
    "out-rtmp-mode-input",
  ) as HTMLSelectElement | null;
  if (rtmpModeInput) {
    rtmpModeInput.value =
      outputConfig.protocol?.type === "rtmp"
        ? outputConfigRtmpMode(outputConfig)
        : matchedPreset?.rtmpMode || "legacy";
  }

  const outUrlInput = document.getElementById(
    "out-rtmp-key-input",
  ) as HTMLInputElement | null;
  if (outUrlInput) {
    outUrlInput.value = matchedPreset ? matchedPreset.inputValue : currentUrl;
  }
  if (detectedProtocol === "srt") {
    const values = parseSrtFields(currentUrl, getDefaultOutputHost());
    (document.getElementById("out-srt-host-input") as HTMLInputElement).value =
      values.host;
    (document.getElementById("out-srt-port-input") as HTMLInputElement).value =
      values.port;
    (
      document.getElementById("out-srt-streamid-input") as HTMLInputElement
    ).value = values.streamId;
    (
      document.getElementById("out-srt-passphrase-input") as HTMLInputElement
    ).value = values.passphrase;
    (
      document.getElementById("out-srt-pbkeylen-input") as HTMLSelectElement
    ).value =
      values.pbkeylen === "24" || values.pbkeylen === "32"
        ? values.pbkeylen
        : "16";
    (
      document.getElementById("out-srt-extra-query-input") as HTMLInputElement
    ).value = values.extraQuery;
  }
  applyOutputProtocolUi(detectedProtocol);

  document
    .getElementById("out-rtmp-key-input")
    ?.classList.remove("input-error");
  document
    .getElementById("out-srt-host-input")
    ?.classList.remove("input-error");
  document.getElementById("out-rtmp-error")?.classList.add("hidden");
  document.getElementById("out-name-input")?.classList.remove("input-error");

  refreshAudioRoutingUi();

  if (outSubmitBtn) {
    outSubmitBtn.disabled = false;
    outSubmitBtn.classList.remove("btn-disabled");
  }

  setupOutputModalProtocolHandlers();
  (document.getElementById("edit-out-modal") as HTMLDialogElement).showModal();
}

export async function editOutBtn(pipeId: string, outId: string): Promise<void> {
  const pipe = state.pipelines.find((p) => p.id === String(pipeId));
  if (!pipe) {
    console.error("Pipeline not found:", pipeId);
    return;
  }

  const output = pipe.outs.find((o) => o.id === String(outId));
  if (!output) {
    console.error("Output not found:", pipeId, outId);
    return;
  }

  await openOutModal("edit", pipe, output);
}

export async function editOutFormBtn(event: Event): Promise<void> {
  event.preventDefault();

  const mode =
    (document.getElementById("out-mode-input") as HTMLInputElement | null)
      ?.value || "edit";
  const pipeId =
    (document.getElementById("out-pipe-id-input") as HTMLInputElement | null)
      ?.value || "";
  const serverUrl =
    (
      document.getElementById(
        "out-server-url-input",
      ) as HTMLSelectElement | null
    )?.value || "";
  const rawInputValue =
    (
      document.getElementById("out-rtmp-key-input") as HTMLInputElement | null
    )?.value.trim() || "";
  const outId =
    (document.getElementById("out-id-input") as HTMLInputElement | null)
      ?.value || "";
  const selectedEncoding =
    (document.getElementById("out-encoding-input") as HTMLSelectElement | null)
      ?.value || "source";
  const outputProtocol =
    (document.getElementById("out-protocol-input") as HTMLSelectElement | null)
      ?.value || "rtmp";
  const rawCodec = (document.getElementById("out-video-codec-input") as HTMLSelectElement | null)?.value || "auto";
  const selectedCodec: OutputVideoCodec = rawCodec === "h264" || rawCodec === "h265" ? rawCodec : "auto";

  const rtmpMode = resolveModalRtmpMode(outputProtocol, serverUrl);
  const config: OutputConfig = {
    video:
      selectedEncoding === "source"
        ? { mode: "source", codec: selectedCodec }
        : { mode: "preset", preset: selectedEncoding, codec: selectedCodec },
    audio:
      modalAudioCtx.modalAudioMode === "subset"
        ? { mode: "selectTracks", tracks: modalAudioCtx.modalAudioSelectedTracks }
        : modalAudioCtx.modalAudioMode === "downmix"
          ? { mode: "downmix", track: modalAudioCtx.modalAudioSelectedTracks[0] ?? 0 }
          : modalAudioCtx.modalAudioMode === "remap"
            ? {
                mode: "remap",
                track:
                  modalAudioCtx.currentModalAudioTracks.length > 1
                    ? parseInt(
                        (
                          document.getElementById(
                            "out-remap-track-input",
                          ) as HTMLSelectElement | null
                        )?.value || "0",
                        10,
                      )
                    : undefined,
                leftChannel: parseInt(
                  (
                    document.getElementById(
                      "out-remap-left-input",
                    ) as HTMLSelectElement | null
                  )?.value || "0",
                  10,
                ),
                rightChannel: parseInt(
                  (
                    document.getElementById(
                      "out-remap-right-input",
                    ) as HTMLSelectElement | null
                  )?.value || "1",
                  10,
                ),
            }
            : { mode: "all" },
    protocol:
      outputProtocol === "rtmp"
        ? { type: "rtmp", mode: rtmpMode }
        : { type: "auto" },
  };
  const data: {
    name: string;
    config: OutputConfig;
    url: string;
    monitoringUrl: string;
  } = {
    name:
      (
        document.getElementById("out-name-input") as HTMLInputElement | null
      )?.value.trim() || "",
    config,
    url: getEffectiveOutputUrlFromModal(),
    monitoringUrl:
      (
        document.getElementById(
          "out-monitoring-url-input",
        ) as HTMLInputElement | null
      )?.value.trim() || "",
  };

  if (serverUrl.includes("${s_prp}")) {
    const params = new URLSearchParams(rawInputValue.split("?")[1]);
    data.url = data.url.replaceAll("${s_prp}", params.get("s_prp") || "");
  }

  const isOutputUrlValid = isValidOutput(data.url);
  const outputErrorField =
    outputProtocol === "srt"
      ? document.getElementById("out-srt-host-input")
      : document.getElementById("out-rtmp-key-input");
  if (isOutputUrlValid) {
    outputErrorField?.classList.remove("input-error");
    document.getElementById("out-rtmp-error")?.classList.add("hidden");
  } else {
    outputErrorField?.classList.add("input-error");
    document.getElementById("out-rtmp-error")?.classList.remove("hidden");
  }

  const isMonitoringUrlValid =
    !data.monitoringUrl || isValidMonitoringUrl(data.monitoringUrl);
  if (isMonitoringUrlValid) {
    document
      .getElementById("out-monitoring-url-input")
      ?.classList.remove("input-error");
    document.getElementById("out-monitoring-error")?.classList.add("hidden");
  } else {
    document
      .getElementById("out-monitoring-url-input")
      ?.classList.add("input-error");
    document.getElementById("out-monitoring-error")?.classList.remove("hidden");
  }

  const isOutNameValid = !!data.name;
  if (isOutNameValid) {
    document.getElementById("out-name-input")?.classList.remove("input-error");
  } else {
    document.getElementById("out-name-input")?.classList.add("input-error");
  }

  if (!isOutputUrlValid || !isMonitoringUrlValid || !isOutNameValid) {
    return;
  }

  const srtPassphraseInput = document.getElementById(
    "out-srt-passphrase-input",
  ) as HTMLInputElement | null;
  const srtPassphrase = srtPassphraseInput?.value.trim() || "";
  const isSrtPassphraseValid =
    outputProtocol !== "srt" ||
    !srtPassphrase ||
    (srtPassphrase.length >= 10 && srtPassphrase.length <= 79);
  srtPassphraseInput?.classList.toggle("input-error", !isSrtPassphraseValid);
  if (!isSrtPassphraseValid) {
    showErrorAlert("SRT egress passphrase must be 10-79 bytes");
    return;
  }

  const res =
    mode === "edit"
      ? await updateOutput(pipeId, outId, data)
      : await createOutput(pipeId, data);

  if (res === null) {
    return;
  }

  upsertDashboardOutputConfig(res.output);
  (
    document.getElementById("edit-out-modal") as HTMLDialogElement | null
  )?.close();
}

export async function deleteOutBtn(
  pipeId: string,
  outId: string,
): Promise<void> {
  const pipe = state.pipelines.find((p) => p.id === String(pipeId));
  if (!pipe) {
    console.error("Pipeline not found:", pipeId);
    return;
  }

  const output = pipe.outs.find((o) => o.id === String(outId));
  if (!output) {
    console.error("Output not found:", pipeId, outId);
    return;
  }

  if (
    !(await confirmInApp({
      title: "Delete Output",
      message: `Delete output "${output.name}"?`,
      confirmLabel: "Delete",
      destructive: true,
    }))
  ) {
    return;
  }

  const res = await deleteOutput(pipeId, outId);

  if (res === null) {
    return;
  }

  removeDashboardOutputConfig(pipeId, outId);
}

export async function addOutBtn(): Promise<void> {
  const pipeId = getUrlParam("p");
  if (!pipeId) {
    console.error("Please select a pipeline first.");
    return;
  }

  const pipe = state.pipelines.find((p) => p.id === pipeId);
  if (!pipe) {
    console.error("Pipeline not found:", pipeId);
    return;
  }

  await openOutModal("create", pipe);
}


window.pipeFormBtn = pipeFormBtn;
window.editOutFormBtn = editOutFormBtn;
window.addOutBtn = addOutBtn;
window.addPipeBtn = addPipeBtn;
window.editPipeBtn = editPipeBtn;
window.deletePipeBtn = deletePipeBtn;
window.onOutEncodingChange = onOutEncodingChange;

void loadStreamKeysOnce();
