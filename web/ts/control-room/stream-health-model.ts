// Stream health: plain-language verdicts about what a publisher sends, read
// from the pipeline's flight recorder. Thresholds follow what the big
// platforms ask of an ingest: a keyframe at least every 2 s (4 s is the
// most any accepts), a steady bitrate, a steady frame rate, and a link
// without sustained loss.

import type { FlightSample, ProbeAudioTrack, StreamQaFindings } from "../core/api-types.js";

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

  const fpsValues = present(ingests.map((ingest) => ingest.fps));
  const fpsTypical = median(fpsValues);
  const fpsNow = last.ingest.fps;
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

/** The engine's alert lines for the picture and sound checks
 * (`media/stream_qa/mod.rs`): a verdict turns red or amber with them. */
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

/** Verdicts from the picture and sound checks; none while they are off. */
export function qaVerdicts(qa: StreamQaFindings | null, nowMs: number): Verdict[] {
  if (qa === null) return [];
  const verdicts: Verdict[] = [];
  const black = held(qa.blackSinceMs, qa.pictureCheckedMs, nowMs);
  const still = held(qa.stillSinceMs, qa.pictureCheckedMs, nowMs);
  if (qa.pictureCheckedMs === null || nowMs - qa.pictureCheckedMs > QA_STALE_MS) {
    verdicts.push({ key: "picture", severity: "warn", title: "Picture not checked", detail: "No keyframe has decoded recently (H.264 and H.265 over SRT, H.264 over RTMP)." });
  } else if (black !== null) {
    verdicts.push({
      key: "picture",
      severity: black >= BLACK_ALERT_SEC ? "bad" : "warn",
      title: `Picture black for ${Math.round(black)} s`,
      detail: "The stream is fine but shows black: check the encoder's scene or camera input.",
    });
  } else if (still !== null && still >= 4) {
    verdicts.push({
      key: "picture",
      severity: still >= STILL_ALERT_SEC ? "warn" : "ok",
      title: `Picture unchanged for ${Math.round(still)} s`,
      detail: "A frozen camera or capture looks like this; so does a slate meant to be still.",
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
    verdicts.push({ key: "sound", severity: "warn", title: "Sound not measured", detail: "Measured on the first AAC track once 3 s of it decode." });
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
