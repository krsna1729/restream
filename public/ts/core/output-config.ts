import type { OutputConfig, OutputView } from "../types.js";

export function parseOutputConfig(
  encoding: string | null | undefined,
): OutputConfig {
  const rawEncoding = String(encoding || "source").trim();
  const rawAudioEncoding = rawEncoding.toLowerCase();
  const compoundMatch = /^([^+]+)\+(.+)$/.exec(rawEncoding);
  let videoEncodingPart = rawEncoding;
  let audioEncodingPart = "";
  if (compoundMatch) {
    videoEncodingPart = compoundMatch[1].trim();
    audioEncodingPart = compoundMatch[2].trim().toLowerCase();
  }

  const audioSource = audioEncodingPart || rawAudioEncoding;
  let audio: OutputConfig["audio"] = { mode: "all" };
  const atrackMatch = /^atrack:(\d+(?:,\d+)*)$/.exec(audioSource);
  const downmixMatch = /^downmix:(\d+)$/.exec(audioSource);
  const remapMatch = /^remap:(\d+):(\d+)(?::(\d+))?$/.exec(audioSource);
  if (atrackMatch) {
    audio = {
      mode: "selectTracks",
      tracks: atrackMatch[1].split(",").map((track) => parseInt(track, 10)),
    };
  } else if (downmixMatch) {
    audio = {
      mode: "downmix",
      track: parseInt(downmixMatch[1], 10),
    };
  } else if (remapMatch) {
    audio = {
      mode: "remap",
      leftChannel: parseInt(remapMatch[1], 10),
      rightChannel: parseInt(remapMatch[2], 10),
      track:
        remapMatch[3] !== undefined ? parseInt(remapMatch[3], 10) : undefined,
    };
  }

  const video =
    compoundMatch || atrackMatch || downmixMatch || remapMatch
      ? videoConfigForEncoding(videoEncodingPart || "source")
      : videoConfigForEncoding(rawEncoding || "source");

  return { video, audio };
}

function videoConfigForEncoding(encoding: string): OutputConfig["video"] {
  const normalized = encoding.trim();
  if (!normalized || normalized === "source") return { mode: "source" };
  if (normalized === "custom") return { mode: "custom" };
  return { mode: "preset", preset: normalized };
}

export function outputConfigToEncoding(config: OutputConfig): string {
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
  config?: OutputConfig;
  encoding?: string | null;
}): OutputConfig {
  return output.config || parseOutputConfig(output.encoding);
}

export function outputViewEncodingLabel(
  output: Pick<OutputView, "config" | "encoding">,
): string {
  return (
    output.encoding || outputConfigToEncoding(normalizeOutputConfig(output))
  );
}
