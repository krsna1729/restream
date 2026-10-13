// What the control room's pipeline and destination editors send. The engine
// validates every write; these functions only shape the operator's choices
// into its bodies and say what the engine would refuse before it is asked.

import type { AudioCaps } from "../core/audio-caps.js";
import type {
  OutputMutationArgs,
  PipelineWrite,
  ProbeAudioTrack,
} from "../core/api-types.js";
import {
  OUTPUT_SERVER_PRESETS,
  detectOutputProtocol,
  effectiveOutputUrl,
  matchOutputServerPreset,
  parseSrtFields,
} from "../core/output-url.js";
import type { SrtFields } from "../core/output-url.js";
import type {
  ConfigOutput,
  ConfigPipeline,
  OutputConfig,
  OutputVideoCodec,
  RtmpOutputMode,
} from "../types.js";
import { passphraseProblem } from "./settings-model.js";

// ── Pipelines ──────────────────────────────────────────────────────────

export interface PipelineDraft {
  readonly name: string;
  /** On create: the key to use ("" lets the engine generate one). */
  readonly streamKey: string;
  /** On edit: ask the engine for a new key (publishers must follow it). */
  readonly rotateKey: boolean;
  readonly source: "live" | "file";
  readonly filename: string;
  readonly loop: boolean;
  readonly startTime: string;
  readonly liveOptimized: boolean;
  readonly targetGopSeconds: number;
  readonly srtMode: "inherit" | "plaintext" | "encrypted";
  readonly srtPassphrase: string;
  readonly srtPbkeylen: 16 | 24 | 32;
  /** "" keeps the engine-wide receive latency. */
  readonly srtLatencyMs: string;
}

export function pipelineDraft(pipeline: ConfigPipeline | null): PipelineDraft {
  const file = pipeline?.fileIngest?.configured ? pipeline.fileIngest : null;
  const policy = pipeline?.srtIngestPolicy;
  return {
    name: pipeline?.name ?? "",
    streamKey: "",
    rotateKey: false,
    source: file ? "file" : "live",
    filename: file?.filename ?? "",
    loop: file?.loop ?? true,
    startTime: file?.startTime ?? "",
    liveOptimized: file?.liveOptimized ?? false,
    targetGopSeconds: file?.targetGopSeconds ?? 2,
    srtMode: policy?.mode ?? "inherit",
    srtPassphrase: policy?.passphrase ?? "",
    srtPbkeylen: policy?.pbkeylen ?? 16,
    srtLatencyMs: policy?.latencyMs == null ? "" : String(policy.latencyMs),
  };
}

function sourceOf(draft: PipelineDraft): Pick<PipelineWrite, "inputSource" | "fileIngest"> {
  if (draft.source === "live") return { inputSource: null, fileIngest: null };
  return {
    inputSource: `file:${draft.filename}`,
    fileIngest: {
      filename: draft.filename,
      loopFlag: draft.loop,
      startTime: draft.startTime.trim(),
      liveOptimized: draft.liveOptimized,
      targetGopSeconds: Math.max(1, Math.round(draft.targetGopSeconds)),
    },
  };
}

/** The create (`stored` null) or update body. An update re-sends the source
 * only when it changed: re-sending it restarts a running file ingest. */
export function pipelineWrite(draft: PipelineDraft, stored: ConfigPipeline | null): PipelineWrite {
  const encrypted = draft.srtMode === "encrypted";
  const latency = draft.srtLatencyMs.trim();
  const source = sourceOf(draft);
  const sourceChanged =
    stored === null ||
    JSON.stringify(source) !== JSON.stringify(sourceOf(pipelineDraft(stored)));
  const key = draft.streamKey.trim();
  return {
    name: draft.name.trim(),
    ...(stored === null ? (key ? { streamKey: key } : {}) : draft.rotateKey ? { streamKey: "" } : {}),
    ...(sourceChanged ? source : {}),
    srtIngestPolicy: {
      mode: draft.srtMode,
      passphrase: encrypted ? draft.srtPassphrase.trim() : null,
      pbkeylen: encrypted ? draft.srtPbkeylen : null,
      latencyMs: latency === "" ? null : Number(latency),
    },
  };
}

/** Why the engine would refuse this pipeline, or null. */
export function pipelineProblem(draft: PipelineDraft): string | null {
  if (!draft.name.trim()) return "A pipeline needs a name.";
  if (draft.source === "file" && !draft.filename) return "Choose the file to ingest.";
  if (draft.srtMode === "encrypted") {
    const problem = passphraseProblem(draft.srtPassphrase);
    if (problem) return problem;
  }
  const latency = draft.srtLatencyMs.trim();
  if (latency !== "") {
    const ms = Number(latency);
    if (!Number.isInteger(ms) || ms < 20 || ms > 8000) {
      return "The SRT receive latency must be 20 to 8000 ms.";
    }
  }
  return null;
}

// ── Destinations ───────────────────────────────────────────────────────

export type OutputProtocol = "rtmp" | "srt" | "hls";
export type AudioMode = OutputConfig["audio"]["mode"];

export interface OutputDraft {
  readonly name: string;
  readonly protocol: OutputProtocol;
  /** A preset server URL, or "" for a full URL typed into `input`. */
  readonly server: string;
  /** The stream key (with a preset) or the full URL (without one). */
  readonly input: string;
  readonly srt: SrtFields;
  /** Only for a custom RTMP server; presets say their own mode. */
  readonly rtmpMode: RtmpOutputMode;
  readonly monitoringUrl: string;
  /** "" sends the source video, a name a transcode profile, null keeps the
   * stored custom video settings (this editor cannot change them). */
  readonly profile: string | null;
  readonly customVideo: OutputConfig["video"] | null;
  readonly codec: OutputVideoCodec;
  readonly audio: AudioMode;
  /** Source track positions: all selected tracks, or the first for the
   * single-track modes. */
  readonly tracks: readonly number[];
  readonly leftChannel: number;
  readonly rightChannel: number;
}

export function outputDraft(output: ConfigOutput | null, defaultHost: string): OutputDraft {
  const url = output?.url ?? "";
  const protocol = (url ? detectOutputProtocol(url) : "rtmp") as OutputProtocol;
  const preset = protocol === "srt" ? null : matchOutputServerPreset(protocol, url);
  const config = output?.config;
  const video = config?.video;
  const audio = config?.audio ?? { mode: "all" };
  return {
    name: output?.name ?? "",
    protocol,
    server: preset?.value ?? "",
    input: preset ? preset.inputValue : url,
    srt: parseSrtFields(url, defaultHost),
    rtmpMode: config?.protocol?.type === "rtmp" ? config.protocol.mode : "legacy",
    monitoringUrl: output?.monitoringUrl ?? "",
    profile: video?.mode === "preset" ? video.preset : video?.mode === "custom" ? null : "",
    customVideo: video?.mode === "custom" ? video : null,
    codec: video && video.mode !== "custom" ? (video.codec ?? "auto") : "auto",
    audio: audio.mode,
    tracks:
      audio.mode === "selectTracks"
        ? audio.tracks
        : audio.mode === "all"
          ? [0]
          : [audio.track ?? 0],
    leftChannel: audio.mode === "remap" ? audio.leftChannel : 0,
    rightChannel: audio.mode === "remap" ? audio.rightChannel : 1,
  };
}

export function outputUrl(draft: OutputDraft): string {
  return effectiveOutputUrl(draft.protocol, draft.server, draft.input, draft.srt);
}

function rtmpMode(draft: OutputDraft): RtmpOutputMode {
  const preset = OUTPUT_SERVER_PRESETS.rtmp.find((candidate) => candidate.value === draft.server);
  return preset?.rtmpMode ?? draft.rtmpMode;
}

function audioConfig(draft: OutputDraft): OutputConfig["audio"] {
  const first = draft.tracks[0] ?? 0;
  switch (draft.audio) {
    case "selectTracks":
      return { mode: "selectTracks", tracks: [...draft.tracks] };
    case "downmix":
      return { mode: "downmix", track: first };
    case "remap":
      return {
        mode: "remap",
        track: first,
        leftChannel: draft.leftChannel,
        rightChannel: draft.rightChannel,
      };
    case "all":
      return { mode: "all" };
  }
}

function videoConfig(draft: OutputDraft): OutputConfig["video"] {
  if (draft.profile === null && draft.customVideo) return draft.customVideo;
  return draft.profile
    ? { mode: "preset", preset: draft.profile, codec: draft.codec }
    : { mode: "source", codec: draft.codec };
}

export function outputWrite(draft: OutputDraft): OutputMutationArgs {
  return {
    name: draft.name.trim(),
    url: outputUrl(draft),
    monitoringUrl: draft.monitoringUrl.trim(),
    config: {
      video: videoConfig(draft),
      audio: audioConfig(draft),
      protocol:
        draft.protocol === "rtmp" ? { type: "rtmp", mode: rtmpMode(draft) } : { type: "auto" },
    },
  };
}

function channels(tracks: readonly ProbeAudioTrack[], position: number): number {
  return tracks[position]?.channels || 2;
}

/** The source tracks a mode sends: for "all", as many as the destination
 * takes, in source order. */
function routed(draft: OutputDraft, caps: AudioCaps, tracks: readonly ProbeAudioTrack[]): number[] {
  if (draft.audio === "all") {
    return tracks.map((_, position) => position).slice(0, caps.maxTracks);
  }
  if (draft.audio === "selectTracks") return [...draft.tracks];
  return [draft.tracks[0] ?? 0];
}

/** How many tracks and channels the pickers offer: what the publisher sends,
 * and never fewer than a stored choice names, so a stored track or channel
 * shows as itself even while nobody publishes. */
export function audioChoiceCounts(
  draft: OutputDraft,
  tracks: readonly ProbeAudioTrack[],
): { tracks: number; channels: number } {
  const first = draft.tracks[0] ?? 0;
  return {
    tracks: Math.max(1, tracks.length, ...draft.tracks.map((track) => track + 1)),
    channels: Math.max(channels(tracks, first), draft.leftChannel + 1, draft.rightChannel + 1),
  };
}

/** What must change before saving, or null. The destination's audio limits
 * are advice (the engine does not enforce them): see `audioNotes`. */
export function outputProblem(draft: OutputDraft): string | null {
  if (!draft.name.trim()) return "A destination needs a name.";
  if (!outputUrl(draft)) {
    return draft.protocol === "srt" ? "An SRT destination needs a host." : "A destination needs a URL.";
  }
  if (draft.protocol === "srt" && draft.srt.passphrase.trim()) {
    const problem = passphraseProblem(draft.srt.passphrase);
    if (problem) return problem;
  }
  if (draft.audio === "selectTracks" && draft.tracks.length === 0) {
    return "Choose at least one audio track to send.";
  }
  return null;
}

/** What the destination will do with the source's audio, and what its
 * limits advise against; none of it stops a save. */
export function audioNotes(
  draft: OutputDraft,
  caps: AudioCaps,
  tracks: readonly ProbeAudioTrack[],
): string[] {
  const notes: string[] = [];
  const sent = routed(draft, caps, tracks);
  if (draft.audio === "all") {
    if (tracks.length > caps.maxTracks) {
      notes.push(
        `Takes ${caps.maxTracks} audio track(s): the other ${tracks.length - caps.maxTracks} are not sent.`,
      );
    } else {
      notes.push(
        tracks.length > 1
          ? `Passes all ${tracks.length} source tracks through as they are.`
          : "Passes the source audio through as it is.",
      );
    }
  }
  if (draft.audio === "selectTracks" && draft.tracks.length > caps.maxTracks) {
    notes.push(
      `This destination takes at most ${caps.maxTracks} audio track(s); ${draft.tracks.length} are selected.`,
    );
  }
  const tooWide = sent.some((position) => channels(tracks, position) > caps.maxChannels);
  if ((draft.audio === "all" || draft.audio === "selectTracks") && tooWide) {
    notes.push(
      `A sent track has more than ${caps.maxChannels} channels, which this destination may refuse: downmix it.`,
    );
  }
  if (draft.audio === "downmix" && tooWide) {
    notes.push(`The track is downmixed to stereo (at most ${caps.maxChannels} channels).`);
  }
  if (tracks.length === 0) {
    notes.push("Nobody publishes now, so the source tracks are unknown; track 1 is the default.");
  }
  return notes;
}
