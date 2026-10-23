// This browser and notifications: the engine's Web Push (a service worker
// shows alerts with no tab open) and in-tab notifications while the
// dashboard is open but hidden. Push needs a secure context (HTTPS, or
// localhost) and a push service the browser can reach.

import { useEffect, useRef } from "react";

import { withBasePath } from "../core/base-path.js";
import { deletePushSubscription, getPushPublicKey, listMyPushSubscriptions, savePushSubscription } from "../core/api.js";
import type { NotifySeverity, OperatorAlert, PushSubscriptionRecord } from "../core/api-types.js";
import { base64UrlToBytes, newlyOpened } from "./notify-model.js";

const IN_TAB_KEY = "restream.notify.inTab";

export type PushSupport = "supported" | "insecure" | "unsupported";

export function pushSupport(): PushSupport {
  if (typeof window === "undefined" || !("Notification" in window)) return "unsupported";
  if (!window.isSecureContext) return "insecure";
  return "serviceWorker" in navigator && "PushManager" in window ? "supported" : "unsupported";
}

export function notificationPermission(): NotificationPermission | "unsupported" {
  return typeof window !== "undefined" && "Notification" in window ? Notification.permission : "unsupported";
}

async function registration(): Promise<ServiceWorkerRegistration> {
  return navigator.serviceWorker.register(withBasePath("/sw.js"), { scope: withBasePath("/") });
}

/** This browser's subscription as the engine knows it, if any. */
export async function currentPush(): Promise<PushSubscriptionRecord | null> {
  if (pushSupport() !== "supported") return null;
  const existing = await (await navigator.serviceWorker.getRegistration(withBasePath("/")))?.pushManager.getSubscription();
  if (!existing) return null;
  const mine = (await listMyPushSubscriptions()) ?? [];
  return mine.find((record) => record.endpoint === existing.endpoint) ?? null;
}

/** Asks for permission, subscribes this browser, and registers it. */
export async function enablePush(minSeverity: NotifySeverity): Promise<PushSubscriptionRecord | string> {
  if (pushSupport() === "insecure") return "Push needs HTTPS (or localhost).";
  if (pushSupport() !== "supported") return "This browser cannot receive push notifications.";
  const permission = await Notification.requestPermission();
  if (permission !== "granted") return "Notifications are blocked for this site in the browser's settings.";
  const key = await getPushPublicKey();
  if (!key) return "The engine has no push key.";
  try {
    const worker = await registration();
    await navigator.serviceWorker.ready;
    const subscription =
      (await worker.pushManager.getSubscription()) ??
      (await worker.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: base64UrlToBytes(key) as BufferSource }));
    const json = subscription.toJSON() as { endpoint: string; keys: { p256dh: string; auth: string } };
    return (await savePushSubscription(json, minSeverity)) ?? "The engine refused the subscription.";
  } catch (error) {
    return `Subscribing failed: ${String(error)}`;
  }
}

export async function disablePush(record: PushSubscriptionRecord | null): Promise<void> {
  const worker = await navigator.serviceWorker.getRegistration(withBasePath("/"));
  await (await worker?.pushManager.getSubscription())?.unsubscribe();
  if (record) await deletePushSubscription(record.id);
}

export function inTabSetting(): NotifySeverity | "off" {
  try {
    const value = window.localStorage.getItem(IN_TAB_KEY);
    return value === "warning" || value === "critical" ? value : "off";
  } catch {
    return "off";
  }
}

export function setInTabSetting(value: NotifySeverity | "off"): void {
  try {
    window.localStorage.setItem(IN_TAB_KEY, value);
  } catch {
    /* storage blocked: the choice lasts this visit */
  }
}

/**
 * While the dashboard is open but hidden, shows a system notification for
 * each newly opened alert (if this browser allowed it and the member turned
 * it on). The engine's push covers closed tabs.
 */
export function useInTabAlertNotifications(alerts: readonly OperatorAlert[] | null): void {
  const seen = useRef<Set<string> | null>(null);
  useEffect(() => {
    if (alerts === null) return;
    const min = inTabSetting();
    const fresh = newlyOpened(seen.current, alerts, min === "off" ? "critical" : min);
    seen.current = new Set(alerts.map((alert) => alert.id));
    if (min === "off" || notificationPermission() !== "granted" || !document.hidden) return;
    for (const alert of fresh.slice(0, 3)) {
      const note = new Notification(`[${alert.severity.toUpperCase()}] ${alert.title}`, {
        body: alert.cause,
        tag: alert.id,
        icon: withBasePath("/logo.png"),
      });
      note.onclick = () => {
        window.focus();
        note.close();
      };
    }
  }, [alerts]);
}
