// Stream health's "Timestamps and sync": per track, what the engine's
// stream inspector read from the source ring over the last minute
// (timestamps that went back or leapt, pictures shown before they decode,
// audio's offset from video and its drift), and each FFmpeg stage's input
// queue. Every value comes from the flight recorder.

import type {
  DecoderHealth,
  StreamInspection,
  TrackInspection,
} from "../core/api-types.js";
import type { Column } from "../ui/data-table.js";
import { DataTable } from "../ui/data-table.js";
import { Panel } from "../ui/panel.js";
import { StatusBadge } from "../ui/tone.js";
import {
  BLOCKED_BAD,
  BLOCKED_WARN,
  DRIFT_WARN_MS,
  OFFSET_WARN_MS,
  trackName,
} from "./stream-health-model.js";

function ms(value: number | undefined): string {
  if (value === undefined) return "--";
  return `${value > 0 ? "+" : ""}${Math.round(value)} ms`;
}

function count(value: number) {
  return value === 0 ? (
    <span className="text-base-content/60">0</span>
  ) : (
    <b className="text-warning">{value}</b>
  );
}

const TRACK_COLUMNS: readonly Column<TrackInspection>[] = [
  {
    id: "track",
    header: "Track",
    cell: (track) => <span className="font-medium">{trackName(track)}</span>,
    priority: 1,
  },
  {
    id: "rate",
    header: "Packets/s",
    align: "right",
    cell: (track) => track.packetsPerSec.toFixed(0),
    priority: 3,
  },
  {
    id: "back",
    header: "Went back",
    align: "right",
    cell: (track) => count(track.backwards),
    sort: (track) => track.backwards,
    priority: 1,
  },
  {
    id: "jumps",
    header: "Leapt",
    align: "right",
    cell: (track) => (
      <span
        title={
          track.largestJumpMs !== undefined
            ? `largest ${ms(track.largestJumpMs)}`
            : undefined
        }
      >
        {count(track.jumps)}
      </span>
    ),
    sort: (track) => track.jumps,
    priority: 1,
  },
  {
    id: "order",
    header: "Out of order",
    align: "right",
    cell: (track) => count(track.ptsBeforeDts),
    priority: 2,
  },
  {
    id: "offset",
    header: "Audio vs video",
    align: "right",
    cell: (track) =>
      track.media === "video" ? (
        <span className="text-base-content/60">reference</span>
      ) : (
        <span
          className={
            Math.abs(track.avOffsetMs ?? 0) >= OFFSET_WARN_MS
              ? "text-warning"
              : undefined
          }
        >
          {ms(track.avOffsetMs)}
        </span>
      ),
    priority: 2,
  },
  {
    id: "drift",
    header: "Clock drift / min",
    align: "right",
    cell: (track) =>
      track.media === "video" ? (
        <span className="text-base-content/60">–</span>
      ) : (
        <span
          className={
            Math.abs(track.clockDriftMsPerMin ?? 0) >= DRIFT_WARN_MS
              ? "text-warning"
              : undefined
          }
        >
          {ms(track.clockDriftMsPerMin)}
        </span>
      ),
    priority: 2,
  },
];

function blockedTone(
  blocked: number | null,
): "ok" | "warn" | "bad" | "neutral" {
  if (blocked === null) return "neutral";
  return blocked >= BLOCKED_BAD
    ? "bad"
    : blocked >= BLOCKED_WARN
      ? "warn"
      : "ok";
}

export function StreamTimeline(props: {
  timeline: StreamInspection | null;
  decoders: readonly DecoderHealth[];
}) {
  const { timeline } = props;
  return (
    <Panel
      id="health-timeline"
      title="Timestamps and sync"
      subtitle={
        timeline
          ? `each track over the last ${Math.round(timeline.windowSec)} s`
          : "per track"
      }
    >
      <div className="flex flex-col gap-4">
        {timeline === null || timeline.tracks.length === 0 ? (
          <p className="text-sm text-base-content/70">
            Reading the source: the engine reads each track's timestamps from
            the source once a second.
          </p>
        ) : (
          <>
            <DataTable
              label="Track timestamps"
              rows={timeline.tracks}
              columns={TRACK_COLUMNS}
              rowKey={(track) => `${track.media}-${track.track}`}
              rowHeight={40}
              cardHeight={150}
            />
            <p className="text-xs text-base-content/70">
              "Went back" and "Leapt" count timestamps that moved against real
              time: a network stall moves both clocks and is not counted. "Out
              of order" counts frames whose presentation time comes before their
              decode time. Audio vs video is how far apart the encoder sends
              audio and video against their timestamps: a steady value is fine,
              over a second makes players wait. Clock drift (AAC tracks, after
              20 s) is how far a track's timestamps move from its own samples
              each minute: that is what slides lip sync.
              {timeline.unread > 0 &&
                ` ${timeline.unread.toLocaleString()} packets went by before they could be read; the counts miss those.`}
            </p>
          </>
        )}
        {props.decoders.length > 0 && (
          <div>
            <h3 className="kicker mb-2">Encoders on this server</h3>
            <ul
              className="flex flex-col gap-1.5"
              aria-label="FFmpeg stage inputs"
            >
              {props.decoders.map((decoder) => (
                <li
                  key={decoder.stage}
                  className="flex flex-wrap items-baseline gap-x-3 gap-y-1 rounded-field bg-base-100 px-3 py-2 text-sm"
                >
                  <span className="font-medium">{decoder.stage}</span>
                  <span className="text-base-content/70">
                    {decoder.lagSec !== null
                      ? `${decoder.lagSec.toFixed(1)} s`
                      : `${decoder.lagSlots} packets`}{" "}
                    behind the source
                  </span>
                  <StatusBadge
                    tone={blockedTone(decoder.inputBlocked)}
                    className="ml-auto"
                  >
                    {decoder.inputBlocked === null
                      ? "measuring"
                      : `input blocked ${Math.round(decoder.inputBlocked * 100)}%`}
                  </StatusBadge>
                </li>
              ))}
            </ul>
          </div>
        )}
      </div>
    </Panel>
  );
}
