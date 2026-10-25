// The publisher's encoder, from what it declared in RTMP `onMetaData`: a
// product name people recognise ("OBS Studio 30.2.3" rather than
// "obs-output module (libobs version 30.2.3)"), and verdicts where what
// arrives falls short of what it is set to send. Pure.

import type { DeclaredEncoder, FlightSample } from "../core/api-types.js";
import type { Verdict } from "./stream-health-model.js";

export interface EncoderIdentity {
  /** A known product's key (its mark), or "other". */
  readonly id: string;
  readonly name: string;
  readonly version: string | null;
  /** What the encoder sent, for the tooltip. */
  readonly raw: string;
}

/** Known encoders by what their `onMetaData` encoder string contains, most
 * specific first. libobs apps other than Streamlabs report as OBS. */
const KNOWN: ReadonlyArray<{ id: string; name: string; match: RegExp }> = [
  { id: "streamlabs", name: "Streamlabs", match: /streamlabs/i },
  {
    id: "obs",
    name: "OBS Studio",
    match: /libobs|^obs\b|obs-output|obs studio/i,
  },
  { id: "ffmpeg", name: "FFmpeg", match: /^lavf|ffmpeg|libavformat/i },
  { id: "vmix", name: "vMix", match: /vmix/i },
  { id: "wirecast", name: "Wirecast", match: /wirecast/i },
  { id: "xsplit", name: "XSplit", match: /xsplit/i },
  { id: "larix", name: "Larix Broadcaster", match: /larix/i },
  { id: "prism", name: "PRISM Live Studio", match: /prism/i },
  { id: "gstreamer", name: "GStreamer", match: /gstreamer|flvmux/i },
  { id: "blackmagic", name: "Blackmagic", match: /blackmagic|atem/i },
  { id: "elgato", name: "Elgato", match: /elgato/i },
  { id: "zoom", name: "Zoom", match: /\bzoom\b/i },
  { id: "teradek", name: "Teradek", match: /teradek/i },
  { id: "haivision", name: "Haivision", match: /haivision|makito/i },
  { id: "elemental", name: "AWS Elemental", match: /elemental|medialive/i },
  { id: "streamyard", name: "StreamYard", match: /streamyard/i },
];

/** libavformat's major version names the FFmpeg release it shipped in
 * (58 is FFmpeg 4, 59 is 5, … 63 is 9). */
function ffmpegRelease(lavf: string): string {
  const major = Number.parseInt(lavf, 10);
  return major >= 58 && major <= 70 ? `${major - 54}.x` : `libavformat ${lavf}`;
}

/** The product behind an `onMetaData` encoder string. */
export function encoderIdentity(raw: string): EncoderIdentity {
  const text = raw.trim();
  const known = KNOWN.find((entry) => entry.match.test(text));
  const libobs = /libobs version ([\d.]+)/i.exec(text);
  const lavf = /^lavf([\d.]+)/i.exec(text);
  const version =
    libobs?.[1] ??
    (lavf
      ? ffmpegRelease(lavf[1])
      : (/\b(\d+(?:\.\d+){1,3})\b/.exec(text)?.[1] ?? null));
  return known
    ? { id: known.id, name: known.name, version, raw: text }
    : {
        id: "other",
        name: text.length > 40 ? `${text.slice(0, 39)}…` : text,
        version: null,
        raw: text,
      };
}

/** "1920×1080 · 60 fps · 6.0 Mb/s video + 160 kb/s audio": what the
 * encoder is set to send, from the parts it declared. */
export function declaredWords(declared: DeclaredEncoder): string {
  const parts: string[] = [];
  if (declared.width && declared.height)
    parts.push(`${declared.width}×${declared.height}`);
  if (declared.frameRate)
    parts.push(`${Number(declared.frameRate.toFixed(2))} fps`);
  const rate = (kbps: number) =>
    kbps >= 1000 ? `${(kbps / 1000).toFixed(1)} Mb/s` : `${kbps} kb/s`;
  const rates = [
    declared.videoKbps ? `${rate(declared.videoKbps)} video` : null,
    declared.audioKbps ? `${rate(declared.audioKbps)} audio` : null,
  ].filter(Boolean);
  if (rates.length) parts.push(rates.join(" + "));
  return parts.join(" · ");
}

/** Arriving below this share of the declared bitrate is a shortfall; below
 * the second, a bad one. */
export const BITRATE_SHORT = 0.75;
export const BITRATE_BAD = 0.5;
/** Arriving below this share of the declared frame rate drops frames; below
 * the second, badly. */
export const FPS_SHORT = 0.9;
export const FPS_BAD = 0.75;
/** Readings needed before comparing: a stream warming up is not short. */
const MIN_READINGS = 5;

function median(values: readonly number[]): number | null {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2
    ? sorted[middle]
    : (sorted[middle - 1] + sorted[middle]) / 2;
}

/** Where what arrives over the last minute falls short of what the encoder
 * declared: bitrate (the encoder or its upload cannot keep up), frame rate
 * (it skips frames), picture size (its output differs from its settings).
 * The frame rate arriving is the stream inspector's video packets a second,
 * one per frame. Nothing without a declaration or enough readings. */
export function declaredVerdicts(
  declared: DeclaredEncoder | null | undefined,
  samples: readonly FlightSample[],
): Verdict[] {
  if (!declared) return [];
  const recent = samples
    .slice(-30)
    .flatMap((sample) => (sample.ingest ? [sample.ingest] : []));
  const who = declared.encoder
    ? encoderIdentity(declared.encoder).name
    : "The encoder";
  const verdicts: Verdict[] = [];

  const declaredBps =
    ((declared.videoKbps ?? 0) + (declared.audioKbps ?? 0)) * 1000;
  const bitrates = recent
    .map((ingest) => ingest.bitrateBps)
    .filter((value): value is number => typeof value === "number");
  const arriving = bitrates.length >= MIN_READINGS ? median(bitrates) : null;
  if (
    declared.videoKbps &&
    arriving !== null &&
    arriving < declaredBps * BITRATE_SHORT
  ) {
    const mbps = (bps: number) => (bps / 1e6).toFixed(1);
    verdicts.push({
      key: "declared-bitrate",
      severity: arriving < declaredBps * BITRATE_BAD ? "bad" : "warn",
      title: `${mbps(arriving)} Mb/s arriving of the ${mbps(declaredBps)} Mb/s ${who} is set to send`,
      detail:
        "The encoder cannot send what it is set to: its upload is too slow or it is dropping frames under load. Lower its bitrate, or check its network and CPU.",
    });
  }

  const rates = recent
    .map(
      (ingest) =>
        ingest.timeline?.tracks.find((track) => track.media === "video")
          ?.packetsPerSec,
    )
    .filter((value): value is number => typeof value === "number" && value > 0);
  const fps = rates.length >= MIN_READINGS ? median(rates) : null;
  if (
    declared.frameRate &&
    fps !== null &&
    fps < declared.frameRate * FPS_SHORT
  ) {
    verdicts.push({
      key: "declared-fps",
      severity: fps < declared.frameRate * FPS_BAD ? "bad" : "warn",
      title: `${Math.round(fps)} fps arriving of the ${Number(declared.frameRate.toFixed(2))} fps ${who} is set to`,
      detail:
        "The encoder is skipping frames: it is overloaded (CPU or GPU) or its capture runs slower than its output. Viewers see stutter.",
    });
  }

  const last = recent[recent.length - 1];
  if (
    declared.width &&
    declared.height &&
    last?.width &&
    last.height &&
    (last.width !== declared.width || last.height !== declared.height)
  ) {
    verdicts.push({
      key: "declared-size",
      severity: "warn",
      title: `${who} declares ${declared.width}×${declared.height} but sends ${last.width}×${last.height}`,
      detail:
        "Its output scaling differs from the settings it reports: check the encoder's output (scaled) resolution.",
    });
  }
  return verdicts;
}
