// Going live: what to check before (and while) a pipeline is on air, and
// how to point an encoder such as OBS at it. Pure; Operate renders it.

import { KEYFRAME_LIMIT_SEC, KEYFRAME_TARGET_SEC } from "./stream-health-model.js";

export type CheckState = "ok" | "warn" | "todo";

export interface Check {
  readonly key: "source" | "keyframes" | "destinations" | "live" | "headroom";
  readonly state: CheckState;
  readonly title: string;
  readonly detail: string;
}

export interface GoLiveFacts {
  /** A publisher is on air. */
  readonly sourceOn: boolean;
  /** The longest keyframe gap in the last minute, when measured. */
  readonly maxGopSec: number | null;
  readonly destinations: number;
  /** Destinations the engine is asked to run, and those delivering. */
  readonly wanted: number;
  readonly delivering: number;
  /** The host's projected load (0..1) and its safe limit. */
  readonly utilization: number | null;
  readonly safeUtilization: number;
}

/** The checks in the order an operator works through them. */
export function goLiveChecks(facts: GoLiveFacts): Check[] {
  const checks: Check[] = [];
  checks.push(
    facts.sourceOn
      ? { key: "source", state: "ok", title: "Source on air", detail: "A publisher is sending to this pipeline." }
      : { key: "source", state: "todo", title: "Connect a source", detail: "Point your encoder at this pipeline (see Set up your encoder), or play a file." },
  );
  if (facts.sourceOn) {
    const gop = facts.maxGopSec;
    checks.push(
      gop === null
        ? { key: "keyframes", state: "warn", title: "Keyframes not measured yet", detail: "Stream health reads them within a minute of the source starting." }
        : gop > KEYFRAME_LIMIT_SEC
          ? { key: "keyframes", state: "warn", title: `Keyframes ${gop.toFixed(1)} s apart`, detail: `Most platforms refuse more than ${KEYFRAME_LIMIT_SEC} s: set the encoder's keyframe interval to ${KEYFRAME_TARGET_SEC} s.` }
          : gop > KEYFRAME_TARGET_SEC * 1.05
            ? { key: "keyframes", state: "warn", title: `Keyframes ${gop.toFixed(1)} s apart`, detail: `Platforms ask for ${KEYFRAME_TARGET_SEC} s; some degrade the stream above it.` }
            : { key: "keyframes", state: "ok", title: "Keyframes every 2 s", detail: "What every platform asks for." },
    );
  }
  checks.push(
    facts.destinations > 0
      ? { key: "destinations", state: "ok", title: `${facts.destinations} destination${facts.destinations === 1 ? "" : "s"} set up`, detail: "Each carries its platform's server and stream key." }
      : { key: "destinations", state: "todo", title: "Add a destination", detail: "Add where to send: a platform preset or any RTMP, SRT or HLS server." },
  );
  if (facts.destinations > 0) {
    checks.push(
      facts.wanted === 0
        ? { key: "live", state: "todo", title: "Not live anywhere", detail: "Start the destinations you want, or Go live on all from Channels." }
        : facts.delivering < facts.wanted
          ? { key: "live", state: "warn", title: `${facts.delivering} of ${facts.wanted} delivering`, detail: facts.sourceOn ? "The rest are starting or retrying: see their state below." : "They wait for the source." }
          : { key: "live", state: "ok", title: `Live on ${facts.wanted}`, detail: "Every wanted destination is delivering." },
    );
  }
  if (facts.utilization !== null) {
    checks.push(
      facts.utilization < facts.safeUtilization
        ? { key: "headroom", state: "ok", title: "The host has room", detail: `Projected load ${Math.round(facts.utilization * 100)}%, safe below ${Math.round(facts.safeUtilization * 100)}%.` }
        : { key: "headroom", state: "warn", title: "The host is near its limit", detail: `Projected load ${Math.round(facts.utilization * 100)}%: adding destinations or renditions risks every stream on it.` },
    );
  }
  return checks;
}

/** OBS's "Custom" service splits an RTMP URL into a server and a key: the
 * key is the last path segment. SRT and others go in whole as the server. */
export function encoderFields(url: string): { readonly server: string; readonly key: string | null } {
  if (!/^rtmps?:\/\//i.test(url)) return { server: url, key: null };
  const cut = url.lastIndexOf("/");
  if (cut <= url.indexOf("//") + 1) return { server: url, key: null };
  return { server: url.slice(0, cut), key: url.slice(cut + 1) };
}

/** The encoder settings platforms ask for, as label and value. */
export const ENCODER_SETTINGS: ReadonlyArray<readonly [string, string]> = [
  ["Keyframe interval", `${KEYFRAME_TARGET_SEC} s`],
  ["Rate control", "CBR"],
  ["Video bitrate", "1080p30: 4.5–6 Mb/s · 720p30: 2.5–4 Mb/s"],
  ["Encoder", "x264 or a hardware H.264 encoder, profile High"],
  ["Audio", "AAC, 48 kHz, 160 kb/s stereo"],
];
