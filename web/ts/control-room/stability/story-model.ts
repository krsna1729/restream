// Turns a run (simulated) or a flight record (measured) into a story: per
// frame, the plant's headline numbers, its queues as coloured cells, and a
// chapter with a caption that says what is happening in plain words, the
// way the reference visualisations narrate an overload.

import type { FlightTrace } from "../../core/api-types.js";
import type { DestinationState, Scenario, SimFrame } from "./sim-engine.js";

export type CellKind = "fresh" | "copy" | "handshake" | "empty";

export interface StoryQueue {
  readonly label: string;
  /** 0..1 of the strip's scale. */
  readonly fill: number;
  /** The strip's cells, oldest first, coloured by what they hold. */
  readonly cells: readonly CellKind[];
  readonly detail: string;
  readonly utilization: number | null;
  readonly states: Partial<Record<DestinationState, number>>;
  /** The queue is full enough that the plant drops or stalls behind it. */
  readonly overflowing: boolean;
}

export type ChapterId = "healthy" | "trigger" | "storm" | "stuck" | "recovering" | "recovered" | "degraded";

export interface Chapter {
  readonly id: ChapterId;
  readonly label: string;
  readonly tone: "ok" | "warn" | "bad" | "info";
  readonly caption: string;
}

export interface StoryFrame {
  readonly atMs: number;
  readonly wanted: number;
  readonly delivering: number;
  /** Reconnect attempts per second (simulated) or retrying outputs (measured). */
  readonly reconnects: number;
  /** Share of work that was re-sent copies or handshakes. */
  readonly wasted: number | null;
  readonly queues: readonly StoryQueue[];
  readonly hottest: number | null;
  readonly trigger: boolean;
  readonly chapter: Chapter;
}

export const CELLS = 40;

/** `cells` cells for a queue holding `parts` of up to `scale`, oldest
 * first: handshakes, then copies, then fresh media, then empty space. */
export function queueCells(parts: { fresh: number; copy: number; handshake: number }, scale: number, cells = CELLS): CellKind[] {
  const total = parts.fresh + parts.copy + parts.handshake;
  const filled = Math.min(cells, Math.round((Math.min(total, scale) / scale) * cells));
  const out: CellKind[] = [];
  if (filled > 0 && total > 0) {
    const handshake = Math.round((parts.handshake / total) * filled);
    const copy = Math.round((parts.copy / total) * filled);
    const fresh = Math.max(0, filled - handshake - copy);
    // Interleave as arrivals would, so the strip reads as a mix.
    const pools: Array<[CellKind, number]> = [
      ["handshake", handshake],
      ["copy", copy],
      ["fresh", fresh],
    ];
    for (let index = 0; out.length < filled; index += 1) {
      const pool = pools[index % 3];
      if (pool[1] > 0) {
        out.push(pool[0]);
        pool[1] -= 1;
      } else if (pools.every((candidate) => candidate[1] === 0)) {
        break;
      }
    }
  }
  while (out.length < cells) out.push("empty");
  return out;
}

const TRIGGER_CAPTION: Record<string, string> = {
  outage: "A platform drops every session. Its destinations retry, and every reconnect is a TCP, TLS and RTMP handshake the shard must do before it sends a byte.",
  spike: "Destinations are added together. Each opens a connection, and the handshakes queue on the shards ahead of the media already flowing.",
  cpuCut: "A neighbour takes CPU from one shard. Its queue grows; destinations behind it stall and resync, and resyncs re-send what they missed.",
};

function simChapter(frame: SimFrame, scenario: Scenario, triggerEndMs: number, history: readonly SimFrame[]): Chapter {
  const share = frame.wanted === 0 ? 1 : frame.delivering / frame.wanted;
  const kind = scenario.events[0]?.kind ?? "outage";
  const active = scenario.events.some((event) =>
    event.kind === "spike"
      ? frame.atMs >= event.atMs && frame.atMs < event.atMs + Math.max(event.spreadMs, 3_000)
      : event.kind === "restartShard"
        ? false
        : frame.atMs >= event.atMs && frame.atMs < event.untilMs,
  );
  const started = frame.atMs >= (scenario.events[0]?.atMs ?? Infinity);
  if (!started) {
    return {
      id: "healthy",
      label: "Healthy",
      tone: "ok",
      caption: `${scenario.summary.split(". ")[0]}. Every destination is live and the shard queues are nearly empty.`,
    };
  }
  if (active && share >= 0.5) {
    return { id: "trigger", label: kind === "outage" ? "Outage" : kind === "spike" ? "Spike" : "CPU cut", tone: "warn", caption: TRIGGER_CAPTION[kind] };
  }
  const sinceEnd = frame.atMs - triggerEndMs;
  if (share < 0.5) {
    if (!active && sinceEnd > 10_000) {
      const wasted = Math.round(frame.wastedShare * 100);
      return {
        id: "stuck",
        label: "Stuck",
        tone: "bad",
        caption: `The trigger ended ${Math.round(sinceEnd / 1000)} s ago, but the loop feeds itself: stalled destinations are torn down and reconnect, and their handshakes and re-sent copies keep the shards full. ${wasted}% of the shards' work is now overhead, not new media.`,
      };
    }
    return {
      id: "storm",
      label: "Overflow",
      tone: "bad",
      caption: "The shard queues are full of handshakes and re-sent copies. Destinations that were fine fall behind them, stall and are torn down, and their reconnects add more handshakes.",
    };
  }
  if (share < 0.95) {
    return { id: "recovering", label: "Recovering", tone: "info", caption: "Backoff spreads the reconnects out, so each shard serves media between handshakes and the queues drain." };
  }
  const recoveredAt = history.find((candidate) => candidate.atMs > triggerEndMs && candidate.delivering / Math.max(1, candidate.wanted) >= 0.95);
  return {
    id: "recovered",
    label: "Recovered",
    tone: "ok",
    caption: `${frame.delivering === frame.wanted ? "Every destination is" : `${frame.delivering} of ${frame.wanted} destinations are`} live again; delivery passed 95%${recoveredAt ? ` ${Math.max(0, Math.round((recoveredAt.atMs - triggerEndMs) / 1000))} s after the trigger ended` : ""}. The queues are empty: backoff spread the reconnects out.`,
  };
}

/** The simulated run as a story. */
export function storyFromSim(frames: readonly SimFrame[], scenario: Scenario): StoryFrame[] {
  const first = scenario.events[0];
  const triggerEndMs = !first
    ? 0
    : first.kind === "spike"
      ? first.atMs + first.spreadMs
      : first.kind === "restartShard"
        ? first.atMs
        : first.untilMs;
  // A strip's scale: 6 s of a shard's work (past the 2 s stall line).
  const scale = scenario.shardCapacity * 6;
  // Reconnects come in waves (attempts time out together): average them
  // over the last 2 s so the number and the line read as a rate.
  const window = Math.max(1, Math.round(2_000 / Math.max(1, (frames[1]?.atMs ?? 500) - (frames[0]?.atMs ?? 0))));
  const smoothed = frames.map((_, index) => {
    const slice = frames.slice(Math.max(0, index - window + 1), index + 1);
    return slice.reduce((sum, frame) => sum + frame.connectsPerSec, 0) / slice.length;
  });
  return frames.map((frame, index) => ({
    atMs: frame.atMs,
    wanted: frame.wanted,
    delivering: frame.delivering,
    reconnects: smoothed[index],
    wasted: frame.wastedShare,
    hottest: Math.max(...frame.shards.map((shard) => shard.utilization)),
    trigger: frame.outage || scenario.events.some((event) => (event.kind === "spike" ? frame.atMs >= event.atMs && frame.atMs < event.atMs + event.spreadMs : event.kind === "cpuCut" && frame.atMs >= event.atMs && frame.atMs < event.untilMs)),
    queues: frame.shards.map((shard, shardIndex) => ({
      label: `shard ${shardIndex}`,
      fill: Math.min(1, (shard.fresh + shard.copy + shard.handshake) / scale),
      cells: queueCells(shard, scale),
      detail: `${shard.queueSec.toFixed(1)} s queued`,
      utilization: shard.utilization,
      states: shard.states,
      overflowing: shard.queueSec * 1000 > scenario.stallMs,
    })),
    chapter: simChapter(frame, scenario, triggerEndMs, frames.slice(0, index + 1)),
  }));
}

/** A measured flight record as a story: the source ring as the queue, the
 * pipeline's outputs as the destinations. */
export function storyFromTrace(trace: FlightTrace): StoryFrame[] {
  return trace.samples.map((sample) => {
    const outputs = sample.outputs;
    const share = outputs.wanted === 0 ? 1 : outputs.delivering / outputs.wanted;
    const ring = sample.ring;
    const ringFill = ring && ring.capacity > 0 ? ring.fill / ring.capacity : 0;
    const chapter: Chapter = !sample.ingest
      ? { id: "degraded", label: "No source", tone: "bad", caption: "No publisher is connected; every wanted destination waits for one." }
      : share >= 0.95
        ? { id: "healthy", label: "Healthy", tone: "ok", caption: `${outputs.delivering} of ${outputs.wanted} wanted destinations deliver; the source ring holds ${Math.round(ringFill * 100)}% of its slots.` }
        : outputs.stalled > 0
          ? { id: "storm", label: "Stalled", tone: "bad", caption: `${outputs.stalled} destination${outputs.stalled === 1 ? " is" : "s are"} stalled behind their feed (worst lag ${outputs.worstFeedLag} units).` }
          : outputs.retrying > 0
            ? { id: "degraded", label: "Retrying", tone: "warn", caption: `${outputs.retrying} destination${outputs.retrying === 1 ? " is" : "s are"} reconnecting under the engine's backoff (${outputs.retryAttempts} attempts so far).` }
            : { id: "degraded", label: "Degraded", tone: "warn", caption: `${outputs.degraded} destination${outputs.degraded === 1 ? " has" : "s have"} a diagnosed fault.` };
    return {
      atMs: sample.atMs,
      wanted: outputs.wanted,
      delivering: outputs.delivering,
      reconnects: outputs.retrying,
      wasted: outputs.offeredBps > 0 ? Math.max(0, 1 - outputs.deliveredBps / outputs.offeredBps) : null,
      hottest: null,
      trigger: false,
      queues: ring
        ? [
            {
              label: "source ring",
              fill: ringFill,
              cells: queueCells({ fresh: ring.fill, copy: 0, handshake: 0 }, ring.capacity),
              detail: `${ring.fill.toLocaleString()} / ${ring.capacity.toLocaleString()} slots · worst reader ${ring.worstLagSlots} behind`,
              utilization: null,
              states: { live: outputs.delivering, stalled: outputs.stalled, backoff: outputs.retrying },
              overflowing: ringFill > 0.9,
            },
          ]
        : [],
      chapter,
    };
  });
}
