import {
  escapeHtml,
  msToHHMMSS,
} from "../../core/utils.js";
import {
  getPublisherQualityAlerts,
  normalizePublisherProtocolLabel,
} from "../publisher-quality.js";
import { syncPublisherMeta } from "./publisher.js";
import type { PipelineView } from "../../types.js";

function formatShortDurationMs(ms: number | null | undefined): string {
  if (ms === null || ms === undefined || !Number.isFinite(ms)) return "--";
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

export function renderPublisherMetaBadgeList(
  publisherMeta: HTMLElement,
  pipe: PipelineView,
  legacyPipelineInputStatusRenderEnabled: boolean,
): void {
  publisherMeta.hidden = !legacyPipelineInputStatusRenderEnabled;

  const publisher = pipe.input.publisher;
  const qualityAlerts = publisher ? getPublisherQualityAlerts(publisher) : [];
  const isHealthy = qualityAlerts.length === 0;
  const unexpectedCount = pipe.input.unexpectedReadersCount || 0;
  const hlsPreview = pipe.hlsPreview;
  const lastDisconnectTitle = [
    pipe.input.lastSessionProtocol
      ? `protocol=${pipe.input.lastSessionProtocol}`
      : "",
    pipe.input.lastFailurePhase ? `phase=${pipe.input.lastFailurePhase}` : "",
    pipe.input.lastDisconnectReason || "",
    pipe.input.lastRemoteAddr ? `remote=${pipe.input.lastRemoteAddr}` : "",
    Number.isFinite(pipe.input.lastSessionBytesReceived as number)
      ? `bytes=${pipe.input.lastSessionBytesReceived}`
      : "",
    pipe.input.lastDisconnectAgeMs !== null
      ? `age=${formatShortDurationMs(pipe.input.lastDisconnectAgeMs)} ago`
      : "",
  ]
    .filter(Boolean)
    .join(" ");
  const hlsPreviewTitle = [
    hlsPreview.active
      ? "Browser preview segmenter is active."
      : "Browser preview segmenter is idle.",
    `segments=${hlsPreview.segments}`,
    `playlistBytes=${hlsPreview.playlistBytes}`,
    `persistentConsumers=${hlsPreview.persistentConsumers}`,
    `lastAccess=${formatShortDurationMs(hlsPreview.lastAccessAgeMs)} ago`,
  ].join(" ");

  syncPublisherMeta(
    publisherMeta,
    [
      pipe.input.time !== null
        ? {
            key: "uptime",
            tagName: "span",
            className: "badge text-sm px-3",
            text: msToHHMMSS(pipe.input.time) || "--",
            title: "",
          }
        : null,
      pipe.input.status === "on" && !pipe.input.probeReady
        ? {
            key: "probe",
            tagName: "span",
            className: "badge badge-warning text-sm px-3",
            text: "Probing",
            title: `Waiting for stream metadata${pipe.input.probePendingMs ? ` (${(pipe.input.probePendingMs / 1000).toFixed(1)}s)` : ""}`,
          }
        : null,
      publisher
        ? {
            key: "protocol",
            tagName: "span",
            className: "badge badge-info text-sm px-3",
            text: normalizePublisherProtocolLabel(publisher.protocol),
            title: "",
          }
        : null,
      publisher?.remoteAddr
        ? {
            key: "remote",
            tagName: "span",
            className: "badge badge-outline font-mono text-sm px-3",
            text: publisher.remoteAddr,
            title: "",
          }
        : null,
      publisher
        ? {
            key: "quality",
            tagName: "button",
            className: `badge text-sm px-3 cursor-pointer ${isHealthy ? "badge-success" : "badge-warning"}`,
            text: isHealthy ? "Healthy" : "Unhealthy",
            title: qualityAlerts.length
              ? qualityAlerts.map((alert: { label: string }) => alert.label).join("\n")
              : "Open publisher health details",
          }
        : null,
      pipe.input.status === "off" && pipe.input.lastDisconnectAt
        ? {
            key: "disconnect",
            tagName: "span",
            className: `badge ${pipe.input.recentDisconnectError ? "badge-warning" : "badge-outline"} text-sm px-3`,
            text: pipe.input.recentDisconnectError
              ? "Last failure"
              : "Last disconnect",
            title: escapeHtml(
              lastDisconnectTitle || "Recent ingest disconnect",
            ),
          }
        : null,
      hlsPreview.active ||
      hlsPreview.segments > 0 ||
      hlsPreview.persistentConsumers > 0
        ? {
            key: "preview",
            tagName: "span",
            className: `badge ${hlsPreview.active ? "badge-success" : "badge-outline"} text-sm px-3`,
            text: hlsPreview.active ? "Preview live" : "Preview idle",
            title: escapeHtml(hlsPreviewTitle),
          }
        : null,
      unexpectedCount > 0
        ? {
            key: "unexpected",
            tagName: "span",
            className: "badge badge-sm badge-error",
            text: `${unexpectedCount} unexpected reader${unexpectedCount === 1 ? "" : "s"}`,
            title: "",
          }
        : null,
    ].filter(Boolean) as {
      key: string;
      tagName: "span" | "button";
      className: string;
      text: string;
      title: string;
    }[],
    pipe.id,
  );
}
