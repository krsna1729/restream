// Stream health: plain-language verdicts about what a publisher sends, read
// from the pipeline's flight recorder. Thresholds follow what the big
// platforms ask of an ingest: a keyframe at least every 2 s (4 s is the
// most any accepts), a steady bitrate, a steady frame rate, and a link
// without sustained loss.

import type { FlightSample } from "../core/api-types.js";

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
