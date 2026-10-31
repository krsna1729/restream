// Stream health: plain-language verdicts about what a publisher sends, read
// from the pipeline's flight recorder. Thresholds follow what the big
// platforms ask of an ingest: a keyframe at least every 2 s (4 s is the
// most any accepts), a steady bitrate, a steady frame rate, and a link
// without sustained loss.

import type { DecoderHealth, FlightSample, IngestHealth, ProbeAudioTrack, StreamInspection, StreamQaFindings, TrackInspection } from "../core/api-types.js";

export type Severity = "ok" | "warn" | "bad";

export interface Verdict {
  readonly key: string;
  readonly severity: Severity;
  readonly title: string;
  readonly detail: string;
}

/** Platforms (YouTube, Twitch, Facebook, Kick) ask for a keyframe at most
 * every 2 s; beyond 4 s most refuse or degrade the stream. */
export const KEYFRAME_TARGET_SEC = 2;
export const KEYFRAME_LIMIT_SEC = 4;

/** The recent samples verdicts are judged over: the last minute at 2 s. */
export const VERDICT_WINDOW = 30;

function median(values: readonly number[]): number | null {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
}

function present(values: ReadonlyArray<number | null | undefined>): number[] {
  return values.filter((value): value is number => typeof value === "number" && Number.isFinite(value));
}

function mbps(bps: number): string {
  return `${(bps / 1e6).toFixed(bps >= 1e7 ? 0 : 1)} Mb/s`;
}

/**
 * Verdicts over the newest `VERDICT_WINDOW` samples, worst first. With no
 * ingest in the newest sample there is one verdict: no source.
 */
export function streamVerdicts(samples: readonly FlightSample[]): Verdict[] {
  const recent = samples.slice(-VERDICT_WINDOW);
  const last = recent[recent.length - 1];
  if (!last) return [];
  if (!last.ingest) {
    return [{ key: "source", severity: "bad", title: "No source", detail: "No publisher is connected to the selected input." }];
  }
  const ingests = recent.map((sample) => sample.ingest).filter((ingest) => ingest !== null);
  const verdicts: Verdict[] = [];

  const maxGop = Math.max(0, ...present(ingests.map((ingest) => ingest.maxGopSec)));
  const meanGop = median(present(ingests.map((ingest) => ingest.gopSec)));
  if (meanGop === null) {
    verdicts.push({ key: "keyframes", severity: "warn", title: "Keyframe interval unknown", detail: "Fewer than two keyframes seen yet." });
  } else if (maxGop > KEYFRAME_LIMIT_SEC) {
    verdicts.push({
      key: "keyframes",
      severity: "bad",
      title: `Keyframes up to ${maxGop.toFixed(1)} s apart`,
      detail: `Platforms want one every ${KEYFRAME_TARGET_SEC} s and most refuse more than ${KEYFRAME_LIMIT_SEC} s. Set the encoder's keyframe interval to ${KEYFRAME_TARGET_SEC} s.`,
    });
  } else if (maxGop > KEYFRAME_TARGET_SEC * 1.05) {
    verdicts.push({
      key: "keyframes",
      severity: "warn",
      title: `Keyframe every ${meanGop.toFixed(1)} s`,
      detail: `Platforms recommend ${KEYFRAME_TARGET_SEC} s; viewers join and recover slower.`,
    });
  } else {
    verdicts.push({ key: "keyframes", severity: "ok", title: `Keyframe every ${meanGop.toFixed(1)} s`, detail: "Within every platform's limit." });
  }

  const rates = present(ingests.map((ingest) => ingest.bitrateBps));
  const typical = median(rates);
  const now = last.ingest.bitrateBps;
  if (typical !== null && now !== null && typical > 0) {
    const swing = Math.max(...rates.map((rate) => Math.abs(rate - typical) / typical));
    if (now < typical * 0.5) {
      verdicts.push({
        key: "bitrate",
        severity: "bad",
        title: `Bitrate fell to ${mbps(now)}`,
        detail: `Half of its typical ${mbps(typical)}: the encoder or its uplink is struggling.`,
      });
    } else if (swing > 0.35) {
      verdicts.push({
        key: "bitrate",
        severity: "warn",
        title: `Bitrate swings ±${Math.round(swing * 100)}%`,
        detail: `Around ${mbps(typical)}. Use constant bitrate (CBR) for live platforms.`,
      });
    } else {
      verdicts.push({ key: "bitrate", severity: "ok", title: `Steady ${mbps(typical)}`, detail: "Bitrate holds within ±35%." });
    }
  }

  const fpsValues = present(ingests.map(arrivingFps));
  const fpsTypical = median(fpsValues);
  const fpsNow = arrivingFps(last.ingest);
  if (fpsTypical !== null && fpsNow !== null) {
    if (fpsNow < fpsTypical * 0.8) {
      verdicts.push({
        key: "fps",
        severity: "warn",
        title: `Frame rate dropped to ${fpsNow.toFixed(0)} fps`,
        detail: `From ${fpsTypical.toFixed(0)} fps: the encoder is dropping frames (CPU or capture).`,
      });
    } else if (fpsNow < 24) {
      verdicts.push({ key: "fps", severity: "warn", title: `${fpsNow.toFixed(0)} fps`, detail: "Below 24 fps looks choppy on every platform." });
    } else {
      verdicts.push({ key: "fps", severity: "ok", title: `${fpsNow.toFixed(fpsNow % 1 ? 2 : 0)} fps`, detail: "Steady frame rate." });
    }
  }

  const loss = median(present(ingests.map((ingest) => ingest.lossPerSec)));
  if (loss !== null) {
    if (loss > 10) {
      verdicts.push({ key: "loss", severity: "bad", title: `Losing ${loss.toFixed(0)} packets/s`, detail: "Sustained loss on the contribution link; raise SRT latency or fix the uplink." });
    } else if (loss > 1) {
      verdicts.push({ key: "loss", severity: "warn", title: `Losing ${loss.toFixed(1)} packets/s`, detail: "SRT retransmits are covering it so far." });
    } else {
      verdicts.push({ key: "loss", severity: "ok", title: "No sustained loss", detail: "The contribution link is clean." });
    }
  }

  const rtt = median(present(ingests.map((ingest) => ingest.rttMs)));
  if (rtt !== null && rtt > 250) {
    verdicts.push({ key: "rtt", severity: "warn", title: `Round trip ${rtt.toFixed(0)} ms`, detail: "A long link: SRT latency should be at least 4× the round trip." });
  }

  const buffer = last.ingest.receiveBuffer;
  if (buffer !== null && buffer > 0.8) {
    verdicts.push({ key: "buffer", severity: "bad", title: `Receive buffer ${Math.round(buffer * 100)}% full`, detail: "The engine is falling behind the publisher." });
  }

  verdicts.push(...qaVerdicts(last.ingest.qa ?? null, last.atMs));
  verdicts.push(...timelineVerdicts(last.ingest.timeline ?? null));
  verdicts.push(...decoderVerdicts(last.decoders ?? []));

  const order: Record<Severity, number> = { bad: 0, warn: 1, ok: 2 };
  return verdicts.sort((a, b) => order[a.severity] - order[b.severity]);
}

/** The worst severity, for a single health chip. */
export function overallSeverity(verdicts: readonly Verdict[]): Severity {
  if (verdicts.some((verdict) => verdict.severity === "bad")) return "bad";
  if (verdicts.some((verdict) => verdict.severity === "warn")) return "warn";
  return "ok";
}

/** The transport a publisher's contribution link runs over, or null when the
 * source has no network link to judge (a file played by the engine, a pull). */
export function contributionTransport(protocol: string | undefined): "SRT" | "TCP" | null {
  switch (protocol?.toLowerCase()) {
    case "srt":
      return "SRT";
    case "rtmp":
    case "rtmps":
      return "TCP";
    default:
      return null;
  }
}

/** H.264 live bitrates the big platforms suggest per height, in Mb/s,
 * at 30 fps and at 50/60 fps (YouTube's published table; Twitch caps
 * 1080p60 near 6 Mb/s, inside the same range). */
const SUGGESTED_MBPS: ReadonlyArray<readonly [number, readonly [number, number], readonly [number, number]]> = [
  [2160, [13, 34], [20, 51]],
  [1440, [6, 13], [9, 18]],
  [1080, [3, 6], [4.5, 9]],
  [720, [1.5, 4], [2.25, 6]],
  [480, [0.5, 2], [0.5, 2]],
  [360, [0.4, 1], [0.4, 1]],
];

/** The bitrate in words against what platforms suggest for the picture:
 * "about right for 1080p30", "low for 1080p60 (platforms suggest 4.5–9 Mb/s)". */
export function bitrateWords(
  bps: number | null | undefined,
  height: number | null | undefined,
  fps: number | null | undefined,
): { readonly severity: Severity; readonly words: string } | null {
  if (!bps || !height || bps <= 0 || height <= 0) return null;
  const row = SUGGESTED_MBPS.find(([rowHeight]) => height >= rowHeight * 0.9) ?? SUGGESTED_MBPS[SUGGESTED_MBPS.length - 1]!;
  const high = (fps ?? 30) > 40;
  const [low, top] = high ? row[2] : row[1];
  const picture = `${row[0]}p${high ? 60 : 30}`;
  const mbps = bps / 1e6;
  if (mbps < low * 0.8) return { severity: "warn", words: `low for ${picture} (platforms suggest ${low}–${top} Mb/s)` };
  if (mbps > top * 1.25) return { severity: "ok", words: `more than ${picture} needs; fine if the uplink keeps up` };
  return { severity: "ok", words: `about right for ${picture}` };
}

/** The alert lines for the picture and sound checks
 * (`crates/restream-health/src/alerts.rs`): a verdict turns red or amber with them. */
export const BLACK_ALERT_SEC = 10;
export const STILL_ALERT_SEC = 30;
export const SILENT_ALERT_SEC = 10;
/** A check older than this says nothing about now. */
const QA_STALE_MS = 10_000;

/** How long a finding has held at `nowMs`, if its check is current. */
function held(sinceMs: number | null, checkedMs: number | null, nowMs: number): number | null {
  if (checkedMs === null || nowMs - checkedMs > QA_STALE_MS || sinceMs === null) return null;
  return Math.max(0, nowMs - sinceMs) / 1000;
}

/** Loudness in words, against what live platforms play back (about -14
 * LUFS; broadcast aims for -23). */
export function loudnessWords(lufs: number | null): { severity: Severity; words: string } | null {
  if (lufs === null) return null;
  if (lufs <= -60) return { severity: "warn", words: "silent" };
  if (lufs > -9) return { severity: "warn", words: "very loud: platforms will turn it down, and it may clip" };
  if (lufs < -30) return { severity: "warn", words: "quiet: viewers will turn it up" };
  return { severity: "ok", words: "a comfortable level for streaming" };
}

/** The checks' measured cost: about 2% of a core with one audio track, and
 * about 0.45% more for each further track FFmpeg decodes. */
export function qaCostWords(audioTracks: number): string {
  const percent = 2 + 0.45 * Math.max(0, audioTracks - 1);
  const tracks = audioTracks === 1 ? "1 audio track" : `${audioTracks} audio tracks`;
  return `about ${percent < 10 ? percent.toFixed(1) : Math.round(percent)}% of a CPU core (${tracks})`;
}

/** The level platforms normalise streams to, LUFS. */
export const LOUDNESS_TARGET_LUFS = -14;
/** A loudness meter's floor, LUFS: the checks call anything at or below
 * -60 silence. */
export const LOUDNESS_FLOOR_LUFS = -60;

/** Where a loudness sits on a meter, 0 (floor) to 1 (0 LUFS). */
export function loudnessFraction(lufs: number): number {
  return Math.min(1, Math.max(0, (lufs - LOUDNESS_FLOOR_LUFS) / -LOUDNESS_FLOOR_LUFS));
}

/** One track's meter from the checks' last measurement: its level and how
 * it reads, or null while the track has no current measurement. */
export function trackMeter(
  track: { silentSinceMs: number | null; loudnessLufs: number } | undefined,
  soundCheckedMs: number | null,
  nowMs: number,
): { fraction: number; severity: Severity; words: string } | null {
  if (track === undefined || soundCheckedMs === null || nowMs - soundCheckedMs > QA_STALE_MS) return null;
  if (track.silentSinceMs !== null) return { fraction: 0, severity: "bad", words: "silent" };
  const reading = loudnessWords(track.loudnessLufs);
  return {
    fraction: loudnessFraction(track.loudnessLufs),
    severity: reading?.severity ?? "ok",
    words: `${track.loudnessLufs.toFixed(0)} LUFS`,
  };
}

/** Frames arriving a second: the stream inspector's video packets a second
 * (one per frame), else the frame rate the stream describes. The stream's
 * own figure is declared (SPS timing, `onMetaData`), so it stays put while
 * an overloaded encoder drops frames; the inspector counts what comes. */
export function arrivingFps(ingest: Pick<IngestHealth, "fps" | "timeline">): number | null {
  const video = ingest.timeline?.tracks.find((track) => track.media === "video");
  if (video && video.packetsPerSec > 0) return Math.round(video.packetsPerSec * 100) / 100;
  return ingest.fps;
}

/** Verdicts from the picture and sound checks; none while they are off. */
export function qaVerdicts(qa: StreamQaFindings | null, nowMs: number): Verdict[] {
  if (qa === null) return [];
  const verdicts: Verdict[] = [];
  const black = held(qa.blackSinceMs, qa.pictureCheckedMs, nowMs);
  const still = held(qa.stillSinceMs, qa.pictureCheckedMs, nowMs);
  if (qa.pictureCheckedMs === null || nowMs - qa.pictureCheckedMs > QA_STALE_MS) {
    verdicts.push({ key: "picture", severity: "warn", title: "Picture not checked", detail: "No keyframe has decoded recently (H.264 or H.265, over SRT or RTMP)." });
  } else if (black !== null) {
    verdicts.push({
      key: "picture",
      severity: black >= BLACK_ALERT_SEC ? "bad" : "warn",
      title: `Picture black for ${Math.round(black)} s`,
      detail: "The stream is fine but shows black: check the encoder's scene or camera input.",
    });
  } else if (still !== null && still >= 4 && qa.stillIsSlate) {
    verdicts.push({
      key: "picture",
      severity: still >= STILL_ALERT_SEC ? "warn" : "ok",
      title: `Slate on for ${Math.round(still)} s`,
      detail: "The picture is still and looks drawn (bars, a card): viewers see a slate. Switch back to the program if it is not planned.",
    });
  } else if (still !== null && still >= 4) {
    verdicts.push({
      key: "picture",
      severity: still >= STILL_ALERT_SEC ? "warn" : "ok",
      title: `Picture frozen for ${Math.round(still)} s`,
      detail: "The camera picture has not changed: a frozen camera or a stuck capture. Restart it on the encoder.",
    });
  } else {
    verdicts.push({ key: "picture", severity: "ok", title: "Picture is moving", detail: "Not black, and changing between keyframes." });
  }
  const silent = held(qa.silentSinceMs, qa.soundCheckedMs, nowMs);
  const loudness = qa.soundCheckedMs !== null && nowMs - qa.soundCheckedMs <= QA_STALE_MS ? loudnessWords(qa.loudnessLufs) : null;
  if (silent !== null) {
    verdicts.push({
      key: "sound",
      severity: silent >= SILENT_ALERT_SEC ? "bad" : "warn",
      title: `Silent for ${Math.round(silent)} s`,
      detail: "The first audio track carries no sound: check the encoder's audio input and mixer.",
    });
  } else if (loudness && qa.loudnessLufs !== null) {
    verdicts.push({ key: "sound", severity: loudness.severity, title: `Sound at ${qa.loudnessLufs.toFixed(0)} LUFS`, detail: `${loudness.words[0].toUpperCase()}${loudness.words.slice(1)}.` });
  } else {
    verdicts.push({ key: "sound", severity: "warn", title: "Sound not measured", detail: "Measured on every AAC track once 3 s of it decode." });
  }
  const tracks = qa.tracks ?? [];
  const silentTracks = tracks
    .map((track, index) => ({ index, silent: held(track.silentSinceMs, qa.soundCheckedMs, nowMs) }))
    .filter((track) => track.index > 0 && track.silent !== null);
  if (silentTracks.length > 0) {
    const longest = Math.max(...silentTracks.map((track) => track.silent ?? 0));
    const names = silentTracks.map((track) => track.index + 1).join(", ");
    verdicts.push({
      key: "tracks",
      severity: longest >= SILENT_ALERT_SEC ? "warn" : "ok",
      title: `${silentTracks.length === 1 ? "Track" : "Tracks"} ${names} silent`,
      detail: "Check that the encoder routes sound to these tracks, or drop them if they are not meant to carry any.",
    });
  }
  return verdicts;
}

const LAYOUT_WORDS: Record<string, string> = { mono: "mono", stereo: "stereo", "5.1": "5.1 surround", "5.1(side)": "5.1 surround", "7.1": "7.1 surround" };

/** One audio track in words: "AAC · 48 kHz · stereo". */
export function audioTrackWords(track: ProbeAudioTrack): string {
  const parts: string[] = [];
  if (track.codec) parts.push(track.codec.toUpperCase());
  if (track.sampleRate) parts.push(`${Number((track.sampleRate / 1000).toFixed(1))} kHz`);
  const layout = track.channelLayout ? (LAYOUT_WORDS[track.channelLayout] ?? track.channelLayout) : null;
  if (layout) parts.push(layout);
  else if (track.channels) parts.push(track.channels === 1 ? "mono" : track.channels === 2 ? "stereo" : `${track.channels} channels`);
  return parts.join(" · ") || "details not reported yet";
}

/** An audio clock drifting this far from its samples each minute is felt
 * as lip sync sliding (ms). */
export const DRIFT_WARN_MS = 200;
/** Audio arriving this far from video makes players wait for one of them
 * (ms). */
export const OFFSET_WARN_MS = 1_000;
/** Captions unseen this long, after the source sent them, have stopped
 * (seconds); this long again and it is bad. */
export const CAPTIONS_STOPPED_SEC = 10;
export const CAPTIONS_GONE_SEC = 60;

/** "CEA-608 and CEA-708": the caption kinds a track carries. */
export function captionKinds(captions: { cea608: boolean; cea708: boolean }): string {
  return [captions.cea608 ? "CEA-608" : null, captions.cea708 ? "CEA-708" : null].filter(Boolean).join(" and ");
}

/** An FFmpeg stage whose input is blocked this share of the time cannot
 * keep up. */
export const BLOCKED_WARN = 0.2;
export const BLOCKED_BAD = 0.5;

/** "Video 1", "Audio 2": a track as the panels name it. */
export function trackName(track: Pick<TrackInspection, "media" | "track">): string {
  return `${track.media === "video" ? "Video" : "Audio"} ${track.track + 1}`;
}

function names(tracks: readonly TrackInspection[]): string {
  return tracks.map(trackName).join(", ");
}

/** Verdicts from the stream inspector's last minute: timestamps that went
 * back or leapt, pictures shown before they decode, an audio clock drifting
 * from its samples, audio sent far from its video. None until it has read
 * the source. */
export function timelineVerdicts(timeline: StreamInspection | null): Verdict[] {
  if (timeline === null || timeline.tracks.length === 0) return [];
  const verdicts: Verdict[] = [];
  const broken = timeline.tracks.filter((track) => track.backwards + track.jumps > 0);
  if (broken.length > 0) {
    const resets = broken.reduce((sum, track) => sum + track.backwards, 0);
    const jumps = broken.reduce((sum, track) => sum + track.jumps, 0);
    const parts = [resets > 0 ? `${resets} went back` : null, jumps > 0 ? `${jumps} leapt` : null].filter(Boolean).join(", ");
    verdicts.push({
      key: "timestamps",
      severity: resets + jumps >= 3 ? "bad" : "warn",
      title: `Timestamps broke on ${names(broken)}`,
      detail: `In the last minute ${parts}. Players stutter, drop audio or stop at each one: look for the encoder (or a relay in front of it) restarting or changing its clock.`,
    });
  } else {
    verdicts.push({ key: "timestamps", severity: "ok", title: "Timestamps steady", detail: "Every track's clock moved with real time over the last minute." });
  }
  const misordered = timeline.tracks.filter((track) => track.ptsBeforeDts > 0);
  if (misordered.length > 0) {
    verdicts.push({
      key: "order",
      severity: "warn",
      title: `Frames shown before they decode on ${names(misordered)}`,
      detail: "Their presentation time comes before their decode time: the encoder or a remuxer writes broken B-frame timing. Players may drop those frames.",
    });
  }
  for (const track of timeline.tracks) {
    const captions = track.captions;
    if (track.media !== "video" || captions === undefined) continue;
    if (captions.lastSeenSec >= CAPTIONS_STOPPED_SEC) {
      verdicts.push({
        key: `captions-${track.track}`,
        severity: captions.lastSeenSec >= CAPTIONS_GONE_SEC ? "bad" : "warn",
        title: `Captions stopped on ${trackName(track)} ${Math.round(captions.lastSeenSec)} s ago`,
        detail: `The source sent ${captionKinds(captions)} captions and stopped: viewers who need them have none. Check the captioner or the encoder's caption input.`,
      });
    } else {
      verdicts.push({
        key: `captions-${track.track}`,
        severity: "ok",
        title: `Captions on ${trackName(track)}`,
        detail: `${captionKinds(captions)} in the video, passed through to every output.${captions.lastTextSec === undefined ? " No caption text yet." : captions.lastTextSec > 60 ? ` No caption text for ${Math.round(captions.lastTextSec / 60)} min.` : ""}`,
      });
    }
  }
  for (const track of timeline.tracks) {
    if (track.media !== "audio") continue;
    const drift = track.clockDriftMsPerMin;
    const offset = track.avOffsetMs;
    if (drift !== undefined && Math.abs(drift) >= DRIFT_WARN_MS) {
      verdicts.push({
        key: `drift-${track.track}`,
        severity: Math.abs(drift) >= OFFSET_WARN_MS ? "bad" : "warn",
        title: `${trackName(track)} clock drifts ${Math.abs(Math.round(drift))} ms a minute`,
        detail: `Its timestamps run ${drift > 0 ? "fast" : "slow"} against its own samples, so players fill gaps or drop audio and lip sync slides. Usually the encoder stamps audio with the system clock while the sound card runs at its own rate: turn on its audio clock sync or resampling.`,
      });
    } else if (offset !== undefined && Math.abs(offset) >= OFFSET_WARN_MS) {
      verdicts.push({
        key: `offset-${track.track}`,
        severity: "warn",
        title: `${trackName(track)} arrives ${(Math.abs(offset) / 1000).toFixed(1)} s ${offset > 0 ? "after" : "before"} the video`,
        detail: "Players hold the other while they wait for it, which adds delay and can stall playback. Check the encoder's interleaving or audio buffer.",
      });
    }
  }
  return verdicts;
}

/** A transcoding stage that cannot keep up: FFmpeg takes its input slower
 * than it arrives. */
export function decoderVerdicts(decoders: readonly DecoderHealth[]): Verdict[] {
  return decoders
    .filter((decoder) => (decoder.inputBlocked ?? 0) >= BLOCKED_WARN)
    .map((decoder) => {
      const blocked = decoder.inputBlocked ?? 0;
      return {
        key: `decoder-${decoder.stage}`,
        severity: blocked >= BLOCKED_BAD ? ("bad" as const) : ("warn" as const),
        title: `${decoder.stage} cannot keep up`,
        detail: `Its FFmpeg took input slower than it arrived ${Math.round(blocked * 100)}% of the time${decoder.lagSec !== null ? `, ${decoder.lagSec.toFixed(1)} s behind the source` : ""}. The host is short of CPU for this encode: lower its preset or resolution, or move work off the host.`,
      };
    });
}
