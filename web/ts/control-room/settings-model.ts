// The rules the Settings workspace applies before it asks the engine to
// change anything, and the words it uses for what the engine reports.

import type { RateLimitAttempt, TranscodeProfile, TranscodeProfiles } from "../core/api-types.js";
import type { ConfigOutput } from "../types.js";

/** The engine's x264 presets and tunes (`TranscodeProfile::validate`). */
export const X264_PRESETS = [
  "ultrafast",
  "superfast",
  "veryfast",
  "faster",
  "fast",
  "medium",
  "slow",
  "slower",
  "veryslow",
  "placebo",
] as const;

export const X264_TUNES = [
  "zerolatency",
  "film",
  "animation",
  "grain",
  "stillimage",
  "psnr",
  "ssim",
  "fastdecode",
  "",
] as const;

/** Profiles outputs may name without configuring them; the page keeps them. */
export const BUILT_IN_PROFILES: readonly string[] = ["h264", "720p", "1080p"];

const SCOPE_LABELS: Record<string, string> = {
  "dashboard-login": "Dashboard login",
  "rtmp-publish": "RTMP publish",
  "srt-publish": "SRT publish",
  "srt-read": "SRT read",
  "hls-publish": "HLS publish",
};

export function scopeLabel(scope: string): string {
  return SCOPE_LABELS[scope] ?? scope;
}

export function banLabel(attempt: RateLimitAttempt): string {
  if (!attempt.banned) return "tracking";
  const seconds = Math.ceil((attempt.banRemainingMs ?? 0) / 1000);
  return seconds > 0 ? `banned ${seconds} s` : "banned";
}

/** The reset that forgets one client in one scope; the engine accepts only
 * its own scope keys, and no scope means every scope. */
export function resetOne(attempt: RateLimitAttempt): { scope: string; ip: string } {
  return { scope: attempt.scope, ip: attempt.ip };
}

/** The engine's dashboard password floor (`MIN_DASHBOARD_PASSWORD_LEN`), in bytes. */
const MIN_PASSWORD_BYTES = 12;

const byteLength = (text: string) => new TextEncoder().encode(text).length;

/** Why a password change cannot be sent yet, or null when it can. */
export function passwordProblem(current: string, next: string, confirm: string): string | null {
  if (!current) return "Enter the current password.";
  if (byteLength(next) < MIN_PASSWORD_BYTES) {
    return `The new password must be at least ${MIN_PASSWORD_BYTES} bytes.`;
  }
  if (next !== confirm) return "The two new passwords differ.";
  return null;
}

/** The SRT passphrase rule for encrypted ingest: 10 to 79 bytes once
 * trimmed, as the engine checks it. */
export function passphraseProblem(passphrase: string): string | null {
  const bytes = byteLength(passphrase.trim());
  return bytes >= 10 && bytes <= 79 ? null : "The passphrase must be 10 to 79 bytes.";
}

export interface ProfileRow {
  readonly name: string;
  readonly profile: TranscodeProfile;
}

export function profileRows(profiles: TranscodeProfiles): readonly ProfileRow[] {
  const builtIn = (name: string) => BUILT_IN_PROFILES.indexOf(name);
  return Object.entries(profiles)
    .map(([name, profile]) => ({ name, profile }))
    .sort((a, b) => {
      const [x, y] = [builtIn(a.name), builtIn(b.name)];
      if (x !== -1 || y !== -1) return (x === -1 ? 99 : x) - (y === -1 ? 99 : y);
      return a.name.localeCompare(b.name);
    });
}

/** `new_profile`, or the first `new_profile_N` no row has. */
export function nextProfileName(rows: readonly ProfileRow[]): string {
  const taken = new Set(rows.map((row) => row.name.trim()));
  if (!taken.has("new_profile")) return "new_profile";
  let suffix = 2;
  while (taken.has(`new_profile_${suffix}`)) suffix += 1;
  return `new_profile_${suffix}`;
}

/** The profiles to save, or why they cannot be: every name is non-empty and
 * unique (a duplicate would silently replace the other), every built-in is
 * kept, and no stored profile an output names is removed here (that output
 * would fall back to the baseline encode without saying so). An output that
 * already names a missing profile does not block unrelated edits. */
export function profilesToSave(
  rows: readonly ProfileRow[],
  outputs: readonly ConfigOutput[],
  stored: TranscodeProfiles,
): { readonly profiles: TranscodeProfiles } | { readonly problem: string } {
  const profiles: TranscodeProfiles = {};
  for (const row of rows) {
    const name = row.name.trim();
    if (!name) return { problem: "Every profile needs a name." };
    if (name in profiles) return { problem: `Two profiles are named "${name}".` };
    profiles[name] = row.profile;
  }
  const missing = BUILT_IN_PROFILES.find((name) => !(name in profiles));
  if (missing) return { problem: `The built-in profile "${missing}" must stay.` };
  for (const output of outputs) {
    const video = output.config.video;
    if (video.mode === "preset" && video.preset in stored && !(video.preset in profiles)) {
      return { problem: `Output "${output.name}" uses profile "${video.preset}"; change it first.` };
    }
  }
  return { profiles };
}

export const NEW_PROFILE: TranscodeProfile = {
  preset: "ultrafast",
  tune: "zerolatency",
  crf: 23,
  gop: 60,
  bframes: 0,
  bitrate: 0,
  maxBitrate: 0,
  width: 0,
  height: 0,
};
