// The browser tab tells you when something is wrong, even from another
// tab: the open-alert count in its title, a coloured dot on its icon, and
// (when this browser asked for it) a short chime for each new critical
// alert. Nothing leaves the page; this is per browser.

import { useEffect, useRef } from "react";

import type { OperatorAlert } from "../core/api-types.js";
import { withBasePath } from "../core/base-path.js";
import { newlyOpened, tabTitle } from "./notify-model.js";
import { WORKSPACE_LABELS } from "./route.js";
import { usePlace } from "./router.js";

const CHIME_KEY = "restream.notify.chime";

export function chimeEnabled(): boolean {
  try {
    return window.localStorage.getItem(CHIME_KEY) === "1";
  } catch {
    return false;
  }
}

export function setChimeEnabled(on: boolean): void {
  try {
    window.localStorage.setItem(CHIME_KEY, on ? "1" : "0");
  } catch {
    /* storage blocked: the choice lasts this visit */
  }
}

/** Two soft tones; browsers allow sound only after the page was used. */
export function chime(): void {
  try {
    const context = new AudioContext();
    const now = context.currentTime;
    for (const [offset, frequency] of [[0, 880], [0.18, 660]] as const) {
      const tone = context.createOscillator();
      const gain = context.createGain();
      tone.frequency.value = frequency;
      gain.gain.setValueAtTime(0.0001, now + offset);
      gain.gain.exponentialRampToValueAtTime(0.15, now + offset + 0.02);
      gain.gain.exponentialRampToValueAtTime(0.0001, now + offset + 0.3);
      tone.connect(gain).connect(context.destination);
      tone.start(now + offset);
      tone.stop(now + offset + 0.32);
    }
    window.setTimeout(() => void context.close(), 800);
  } catch {
    /* no audio here */
  }
}

/** The logo with a dot in the alert's colour, as a data URL. */
function badgeIcon(source: HTMLImageElement, color: string): string | null {
  try {
    const canvas = document.createElement("canvas");
    canvas.width = canvas.height = 64;
    const context = canvas.getContext("2d");
    if (!context) return null;
    context.drawImage(source, 0, 0, 64, 64);
    context.beginPath();
    context.arc(48, 16, 14, 0, Math.PI * 2);
    context.fillStyle = color;
    context.fill();
    context.lineWidth = 4;
    context.strokeStyle = "#ffffff";
    context.stroke();
    return canvas.toDataURL("image/png");
  } catch {
    return null;
  }
}

export function useTabAlerts(alerts: readonly OperatorAlert[] | null): void {
  const place = usePlace();
  const seen = useRef<Set<string> | null>(null);
  const logo = useRef<HTMLImageElement | null>(null);

  useEffect(() => {
    document.title = tabTitle(`${WORKSPACE_LABELS[place.workspace]} · Restream`, alerts ?? []);
  }, [alerts, place.workspace]);

  useEffect(() => {
    if (alerts === null) return;
    const fresh = newlyOpened(seen.current, alerts, "critical");
    seen.current = new Set(alerts.map((alert) => alert.id));
    if (fresh.length > 0 && chimeEnabled()) chime();
  }, [alerts]);

  const tone = alerts === null || alerts.length === 0 ? null : alerts.some((alert) => alert.severity === "critical") ? "#dc2626" : "#d97706";
  useEffect(() => {
    const link = document.querySelector<HTMLLinkElement>('link[rel="icon"]');
    if (!link) return;
    const plain = withBasePath("/logo.png");
    if (tone === null) {
      link.href = plain;
      return;
    }
    const apply = () => {
      const url = logo.current && badgeIcon(logo.current, tone);
      if (url) link.href = url;
    };
    if (logo.current?.complete) apply();
    else {
      const image = new Image();
      image.onload = () => {
        logo.current = image;
        apply();
      };
      image.src = plain;
    }
  }, [tone]);
}
