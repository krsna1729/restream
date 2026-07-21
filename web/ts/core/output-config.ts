import type {
  OutputConfig,
  OutputProtocolConfig,
  OutputVideoCodec,
  OutputView,
} from "../types.js";
import { type UnknownRecord, isRecord } from "./validators.js";

function defaultOutputConfig(): OutputConfig {
  return {
    video: { mode: "source", codec: "auto" },
    audio: { mode: "all" },
    protocol: { type: "auto" },
  };
}

function normalizeVideoCodec(value: unknown): OutputVideoCodec {
  return value === "h264" || value === "h265" ? value : "auto";
}

function normalizeIndex(value: unknown, fallback = 0): number {
  return typeof value === "number" &&
    Number.isFinite(value) &&
    Number.isInteger(value) &&
    value >= 0
    ? value
    : fallback;
}

function normalizeVideoConfig(value: unknown): OutputConfig["video"] {
  if (!isRecord(value)) return defaultOutputConfig().video;

  switch (value.mode) {
    case "source":
      return { mode: "source", codec: normalizeVideoCodec(value.codec) };
    case "custom":
      return { mode: "custom" };
    case "preset":
      return typeof value.preset === "string" && value.preset.length > 0
        ? {
            mode: "preset",
            preset: value.preset,
            codec: normalizeVideoCodec(value.codec),
          }
        : defaultOutputConfig().video;
    default:
      return defaultOutputConfig().video;
  }
}

function normalizeAudioConfig(value: unknown): OutputConfig["audio"] {
  if (!isRecord(value)) return { mode: "all" };

  switch (value.mode) {
    case "all":
      return { mode: "all" };
    case "selectTracks":
      return {
        mode: "selectTracks",
        tracks: Array.isArray(value.tracks)
          ? value.tracks
              .filter(
                (track): track is number =>
                  typeof track === "number" &&
                  Number.isFinite(track) &&
                  Number.isInteger(track) &&
                  track >= 0,
              )
          : [],
      };
    case "downmix":
      return { mode: "downmix", track: normalizeIndex(value.track) };
    case "remap":
      return {
        mode: "remap",
        track: normalizeIndex(value.track),
        leftChannel: normalizeIndex(value.leftChannel),
        rightChannel: normalizeIndex(value.rightChannel, 1),
      };
    default:
      return { mode: "all" };
  }
}

function normalizeProtocolConfig(value: unknown): OutputProtocolConfig {
  if (!isRecord(value) || value.type === "auto") return { type: "auto" };
  if (value.type === "rtmp") {
    return {
      type: "rtmp",
      mode: value.mode === "enhanced" ? "enhanced" : "legacy",
    };
  }
  return { type: "auto" };
}

export function outputConfigRtmpMode(config: OutputConfig): "legacy" | "enhanced" {
  return config.protocol?.type === "rtmp" ? config.protocol.mode : "legacy";
}

export function outputConfigStageLabel(config: OutputConfig): string {
  const video = outputConfigVideoLabel(config);
  const audio = outputConfigAudioOperation(config);
  if (!audio) return video;
  if (config.video.mode === "source") return audio;
  return `${video}+${audio}`;
}

export function outputConfigVideoLabel(config: OutputConfig): string {
  switch (config.video.mode) {
    case "source":
      return "source";
    case "custom":
      return "custom";
    case "preset":
      return config.video.preset;
  }
}

export function outputConfigAudioOperation(
  config: OutputConfig,
): string | null {
  switch (config.audio.mode) {
    case "all":
      return null;
    case "selectTracks":
      return config.audio.tracks.length > 0
        ? `atrack:${config.audio.tracks.join(",")}`
        : null;
    case "downmix":
      return `downmix:${config.audio.track}`;
    case "remap":
      return config.audio.track === undefined || config.audio.track === 0
        ? `remap:${config.audio.leftChannel}:${config.audio.rightChannel}`
        : `remap:${config.audio.leftChannel}:${config.audio.rightChannel}:${config.audio.track}`;
  }
}

export function normalizeOutputConfig(
  output: { config?: unknown } | null | undefined,
): OutputConfig {
  const rawOutput: UnknownRecord = isRecord(output) ? output : {};
  const rawConfig: UnknownRecord = isRecord(rawOutput.config)
    ? rawOutput.config
    : {};
  return {
    video: normalizeVideoConfig(rawConfig.video),
    audio: normalizeAudioConfig(rawConfig.audio),
    protocol: normalizeProtocolConfig(rawConfig.protocol),
  };
}

export function outputViewEncodingLabel(
  output: Pick<OutputView, "config">,
): string {
  return outputConfigStageLabel(normalizeOutputConfig(output));
}
