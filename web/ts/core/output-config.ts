import type { OutputConfig, OutputView } from "../types.js";

function defaultOutputConfig(): OutputConfig {
  return {
    video: { mode: "source", codec: "auto" },
    audio: { mode: "all" },
    protocol: { type: "auto" },
  };
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

export function normalizeOutputConfig(output: {
  config?: OutputConfig | null;
}): OutputConfig {
  const config = { ...defaultOutputConfig(), ...(output.config || {}) };
  switch (config.video.mode) {
    case "source":
      config.video = { ...config.video, codec: config.video.codec || "auto" };
      break;
    case "preset":
      config.video = { ...config.video, codec: config.video.codec || "auto" };
      break;
    case "custom":
      break;
  }
  return config;
}

export function outputViewEncodingLabel(
  output: Pick<OutputView, "config">,
): string {
  return outputConfigStageLabel(normalizeOutputConfig(output));
}
