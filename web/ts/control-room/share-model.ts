// Share links as the Share panel shows them: the absolute watch URL under
// whatever prefix the dashboard is served from, the embed snippet, and
// whether a link still works. Pure.

import type { ShareLink } from "../core/api-types.js";

/** The watch page URL, given the dashboard's own page URL (any path,
 * query or hash; a proxy prefix is kept). */
export function watchUrl(pageUrl: string, token: string): string {
  const url = new URL(pageUrl);
  const base = url.pathname.replace(/[^/]*$/, "");
  return `${url.origin}${base}watch/${token}`;
}

/** An iframe that plays the link anywhere. */
export function embedSnippet(watch: string, title: string): string {
  const safe = title.replace(/[&"<>]/g, (ch) => ({ "&": "&amp;", '"': "&quot;", "<": "&lt;", ">": "&gt;" })[ch] ?? ch);
  return `<iframe src="${watch}" title="${safe}" width="960" height="540" allow="autoplay; fullscreen; picture-in-picture" allowfullscreen style="border:0;max-width:100%;aspect-ratio:16/9;height:auto"></iframe>`;
}

export type LinkState = "live" | "expiring" | "expired";

/** Expiring: under a day left. */
export function linkState(link: Pick<ShareLink, "expiresAtMs">, nowMs: number): LinkState {
  if (link.expiresAtMs === null) return "live";
  if (link.expiresAtMs <= nowMs) return "expired";
  return link.expiresAtMs - nowMs < 86_400_000 ? "expiring" : "live";
}

export const LIFETIMES: ReadonlyArray<{ label: string; hours: number | null }> = [
  { label: "1 hour", hours: 1 },
  { label: "1 day", hours: 24 },
  { label: "7 days", hours: 168 },
  { label: "30 days", hours: 720 },
  { label: "Until revoked", hours: null },
];
