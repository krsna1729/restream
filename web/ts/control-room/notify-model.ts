// Notifications in the control room: the channel kinds an admin can add,
// how a pasted URL suggests its kind, what a draft is missing, which alerts
// are new since the last look (for in-tab notifications), and the delivery
// log's verdict. Pure.

import type { ChannelKind, NotifyChannelDraft, NotifyDelivery, NotifySeverity, OperatorAlert } from "../core/api-types.js";

export interface KindInfo {
  readonly label: string;
  readonly placeholder: string;
  readonly hint: string;
}

export const CHANNEL_KINDS: Record<ChannelKind, KindInfo> = {
  slack: {
    label: "Slack",
    placeholder: "https://hooks.slack.com/services/…",
    hint: "An Incoming Webhook URL from a Slack app; messages post to its channel.",
  },
  discord: {
    label: "Discord",
    placeholder: "https://discord.com/api/webhooks/…",
    hint: "A channel webhook URL (Channel settings → Integrations → Webhooks).",
  },
  webhook: {
    label: "Webhook",
    placeholder: "https://ops.example.com/restream-alerts",
    hint: "A signed JSON POST (restream.notify.v1) for PagerDuty, Opsgenie or your own service. The signing secret is shown once.",
  },
};

/** The kind a pasted URL is, when its host says so. */
export function guessKind(url: string): ChannelKind | null {
  const lower = url.trim().toLowerCase();
  if (lower.startsWith("https://hooks.slack.com/")) return "slack";
  if (/^https:\/\/(ptb\.|canary\.)?discord(app)?\.com\/api\/webhooks\//.test(lower)) return "discord";
  return null;
}

/** What stops a draft from saving, mirroring the engine's checks. */
export function channelProblem(draft: NotifyChannelDraft): string | null {
  if (!draft.name.trim()) return "Name the channel.";
  if (draft.name.trim().length > 80) return "A name is at most 80 characters.";
  const match = draft.target.trim().match(/^(https?):\/\/([^/?#]+)/i);
  if (!match) return "The target must be an http(s) URL.";
  return null;
}

export function severityAtLeast(severity: NotifySeverity, min: NotifySeverity): boolean {
  return min === "warning" || severity === "critical";
}

/**
 * The alerts to announce in this tab: open now, not open at the last look,
 * and at least `min`. The first look (`previous` null) only learns.
 */
export function newlyOpened(
  previous: ReadonlySet<string> | null,
  alerts: readonly OperatorAlert[],
  min: NotifySeverity,
): OperatorAlert[] {
  if (previous === null) return [];
  return alerts.filter((alert) => !previous.has(alert.id) && severityAtLeast(alert.severity, min));
}

/** A VAPID key (base64url) as the bytes `pushManager.subscribe` takes. */
export function base64UrlToBytes(value: string): Uint8Array {
  const base64 = value.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(value.length / 4) * 4, "=");
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index);
  return bytes;
}

export interface ChannelHealth {
  readonly sent: number;
  readonly failed: number;
  /** The latest delivery, newest first in the log. */
  readonly last: NotifyDelivery | null;
}

/** Per target, how its deliveries in the log went. */
export function channelHealth(deliveries: readonly NotifyDelivery[]): Map<string, ChannelHealth> {
  const health = new Map<string, ChannelHealth>();
  for (const delivery of deliveries) {
    const current = health.get(delivery.targetId) ?? { sent: 0, failed: 0, last: null };
    health.set(delivery.targetId, {
      sent: current.sent + (delivery.ok ? 1 : 0),
      failed: current.failed + (delivery.ok ? 0 : 1),
      last: current.last ?? delivery,
    });
  }
  return health;
}
