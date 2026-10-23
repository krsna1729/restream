// A small, deterministic model of restream's egress under stress: why a
// short outage can leave the plant stuck long after it ends (a metastable
// failure), and which reconnect policy recovers.
//
// The model is fluid, stepped every `TICK_MS`. Each egress shard is one
// service center with a work capacity per second (its share of the
// calibrated egress limit). Its queue holds three kinds of work, which the
// Stability Lab colours like the reference visualisations:
//   - fresh: a live destination's steady sending;
//   - copy: work re-sent after a stall (a resync replays the backlog);
//   - handshake: a reconnect attempt (TCP/TLS/RTMP setup costs as much as
//     seconds of sending).
// A destination whose shard queue is older than the stall timeout is
// stalled: the engine resyncs it (copies) and, past the give-up timeout,
// tears it down and retries it under the reconnect policy. Retries are
// handshakes, so a busy shard makes more work for itself: the loop that
// keeps a system stuck after its trigger is gone.
//
// Pure and seeded: the same scenario and seed give the same trace, in a
// worker or a test.

export type RetryPolicyKind = "engine" | "fixed" | "immediate";

export interface RetryPolicy {
  readonly kind: RetryPolicyKind;
  /** engine: first backoff; fixed: the constant wait. */
  readonly baseMs: number;
  /** engine: the cap. */
  readonly maxMs: number;
  /** After this many attempts a destination gives up (`retriesExhausted`). */
  readonly maxRetries: number;
}

export interface Scenario {
  readonly id: string;
  readonly title: string;
  readonly summary: string;
  readonly durationMs: number;
  readonly shards: number;
  /** Destinations at the start; a `spike` event adds more. */
  readonly destinations: number;
  /** Work units a shard serves per second (its calibrated capacity). */
  readonly shardCapacity: number;
  /** Work a live destination needs per second (its packet rate × cost). */
  readonly sendCost: number;
  /** Work one reconnect attempt costs, in units. */
  readonly handshakeCost: number;
  /** Queue age past which a destination is stalled and resynced. */
  readonly stallMs: number;
  /** Stalled this long, the destination is torn down and retried. */
  readonly giveUpMs: number;
  /** A failed connect takes this long to fail (a timeout). */
  readonly connectTimeoutMs: number;
  /** The share of destinations on the platform that goes down. */
  readonly outageShare: number;
  readonly events: readonly ScenarioEvent[];
}

export type ScenarioEvent =
  | { readonly atMs: number; readonly kind: "outage"; readonly untilMs: number }
  /** `spreadMs`: the starts are paced over this long (Channels' bulk
   * go-live runs a few at a time); 1 s means all at once. */
  | { readonly atMs: number; readonly kind: "spike"; readonly add: number; readonly spreadMs: number }
  | { readonly atMs: number; readonly kind: "cpuCut"; readonly shard: number; readonly factor: number; readonly untilMs: number }
  | { readonly atMs: number; readonly kind: "restartShard"; readonly shard: number };

export type DestinationState = "live" | "stalled" | "connecting" | "backoff" | "failed";

export interface ShardFrame {
  /** Seconds of work queued (its age at the head). */
  readonly queueSec: number;
  /** Units queued, by kind. */
  readonly fresh: number;
  readonly copy: number;
  readonly handshake: number;
  /** Fraction of capacity used over the last tick (0..1). */
  readonly utilization: number;
  readonly states: Readonly<Record<DestinationState, number>>;
}

/** One recorded instant of the simulated plant. */
export interface SimFrame {
  readonly atMs: number;
  readonly wanted: number;
  /** Live and not stalled: actually reaching viewers. */
  readonly delivering: number;
  readonly connectsPerSec: number;
  readonly failuresPerSec: number;
  /** Share of served work that was re-sent copies or handshakes, not new media. */
  readonly wastedShare: number;
  readonly shards: readonly ShardFrame[];
  readonly outage: boolean;
}

export const TICK_MS = 100;
/** Frames are kept every this often (the trace's sample interval). */
export const FRAME_MS = 500;

/** mulberry32: small, fast, deterministic. */
export function seeded(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** The wait before `attempt` (1-based) under `policy`, with `jitter` in
 * [0, 1): the engine's equal jitter keeps half the backoff fixed. */
export function backoffMs(policy: RetryPolicy, attempt: number, jitter: number): number {
  switch (policy.kind) {
    case "immediate":
      return 0;
    case "fixed":
      return policy.baseMs;
    case "engine": {
      const shift = Math.min(attempt, 16);
      const capped = Math.min(policy.maxMs, policy.baseMs * 2 ** shift);
      const half = capped / 2;
      return half + jitter * half;
    }
  }
}

interface Destination {
  state: DestinationState;
  shard: number;
  onOutagePlatform: boolean;
  attempts: number;
  /** connecting: when the attempt resolves; backoff: when it starts. */
  untilMs: number;
  stalledSinceMs: number;
}

interface ShardQueue {
  fresh: number;
  copy: number;
  handshake: number;
  capacityFactor: number;
  served: number;
}

/** Runs `scenario` under `policy` and returns its frames, every `FRAME_MS`. */
export function simulate(scenario: Scenario, policy: RetryPolicy, seed = 1): SimFrame[] {
  const random = seeded(seed);
  const destinations: Destination[] = [];
  const add = (count: number, atMs: number, spreadMs = 1000) => {
    for (let index = 0; index < count; index += 1) {
      destinations.push({
        // New destinations connect within a second of being added: each
        // attempt is a handshake on its shard.
        state: "backoff",
        shard: destinations.length % scenario.shards,
        onOutagePlatform: random() < scenario.outageShare,
        attempts: 0,
        untilMs: atMs + random() * spreadMs,
        stalledSinceMs: 0,
      });
    }
  };
  const queues: ShardQueue[] = Array.from({ length: scenario.shards }, () => ({ fresh: 0, copy: 0, handshake: 0, capacityFactor: 1, served: 0 }));
  add(scenario.destinations, 0);
  // The plant starts settled: everyone live.
  for (const destination of destinations) destination.state = "live";

  const frames: SimFrame[] = [];
  let connects = 0;
  let failures = 0;
  let wasted = 0;
  let servedTotal = 0;
  const outageAt = (atMs: number) =>
    scenario.events.some((event) => event.kind === "outage" && atMs >= event.atMs && atMs < event.untilMs);
  const handled = new Set<ScenarioEvent>();

  for (let atMs = 0; atMs <= scenario.durationMs; atMs += TICK_MS) {
    const dt = TICK_MS / 1000;
    const outage = outageAt(atMs);
    for (const event of scenario.events) {
      if (handled.has(event) || atMs < event.atMs) continue;
      handled.add(event);
      if (event.kind === "spike") add(event.add, atMs, event.spreadMs);
      if (event.kind === "restartShard") {
        const queue = queues[event.shard];
        if (queue) {
          queue.fresh = 0;
          queue.copy = 0;
          queue.handshake = 0;
        }
      }
      if (event.kind === "outage") {
        // The platform drops its sessions: each goes straight to retrying.
        for (const destination of destinations) {
          if (destination.onOutagePlatform && destination.state === "live") {
            destination.state = "backoff";
            destination.attempts = 0;
            destination.untilMs = atMs + backoffMs(policy, 0, random());
            failures += 1;
          }
        }
      }
    }
    for (const queue of queues) queue.capacityFactor = 1;
    for (const event of scenario.events) {
      if (event.kind === "cpuCut" && atMs >= event.atMs && atMs < event.untilMs) {
        const queue = queues[event.shard];
        if (queue) queue.capacityFactor = event.factor;
      }
    }

    // Arrivals: steady sending, copies for stalled destinations, and
    // handshakes for the attempts that start now.
    for (const destination of destinations) {
      const queue = queues[destination.shard];
      switch (destination.state) {
        case "live":
          queue.fresh += scenario.sendCost * dt;
          break;
        case "stalled":
          // A resync re-sends what the destination missed, on top of new media.
          queue.fresh += scenario.sendCost * dt;
          queue.copy += scenario.sendCost * dt * 1.5;
          break;
        case "backoff":
          if (atMs >= destination.untilMs) {
            destination.state = "connecting";
            destination.attempts += 1;
            destination.untilMs = atMs + scenario.connectTimeoutMs;
            queue.handshake += scenario.handshakeCost;
            connects += 1;
          }
          break;
        default:
          break;
      }
    }

    // Service: each shard drains handshakes first (the event loop accepts
    // connections eagerly), then copies, then fresh media.
    const ages: number[] = [];
    for (const queue of queues) {
      let budget = (scenario.shardCapacity * queue.capacityFactor) * dt;
      const before = queue.fresh + queue.copy + queue.handshake;
      for (const kind of ["handshake", "copy", "fresh"] as const) {
        const take = Math.min(queue[kind], budget);
        queue[kind] -= take;
        budget -= take;
        if (kind !== "fresh") wasted += take;
        servedTotal += take;
      }
      queue.served = before === 0 ? 0 : 1 - budget / (scenario.shardCapacity * queue.capacityFactor * dt);
      const backlog = queue.fresh + queue.copy + queue.handshake;
      ages.push(backlog / Math.max(1e-9, scenario.shardCapacity * queue.capacityFactor));
    }

    // Outcomes: connects resolve, live destinations stall behind an old
    // queue, stalled ones are torn down past the give-up timeout.
    for (const destination of destinations) {
      const ageMs = ages[destination.shard] * 1000;
      if (destination.state === "connecting" && atMs >= destination.untilMs) {
        const refused = (destination.onOutagePlatform && outage) || ageMs > scenario.connectTimeoutMs;
        if (refused) {
          failures += 1;
          if (destination.attempts >= policy.maxRetries) {
            destination.state = "failed";
          } else {
            destination.state = "backoff";
            destination.untilMs = atMs + backoffMs(policy, destination.attempts, random());
          }
        } else {
          destination.state = "live";
          destination.attempts = 0;
        }
      } else if (destination.state === "live" && ageMs > scenario.stallMs) {
        destination.state = "stalled";
        destination.stalledSinceMs = atMs;
      } else if (destination.state === "stalled") {
        if (ageMs <= scenario.stallMs / 2) {
          destination.state = "live";
        } else if (atMs - destination.stalledSinceMs > scenario.giveUpMs) {
          destination.state = "backoff";
          destination.untilMs = atMs + backoffMs(policy, destination.attempts, random());
          failures += 1;
        }
      }
    }

    if (atMs % FRAME_MS === 0) {
      const perShard = queues.map(() => ({ live: 0, stalled: 0, connecting: 0, backoff: 0, failed: 0 }));
      let delivering = 0;
      for (const destination of destinations) {
        perShard[destination.shard][destination.state] += 1;
        if (destination.state === "live") delivering += 1;
      }
      const seconds = FRAME_MS / 1000;
      frames.push({
        atMs,
        wanted: destinations.length,
        delivering,
        connectsPerSec: connects / seconds,
        failuresPerSec: failures / seconds,
        wastedShare: servedTotal === 0 ? 0 : wasted / servedTotal,
        outage,
        shards: queues.map((queue, index) => ({
          queueSec: ages[index],
          fresh: queue.fresh,
          copy: queue.copy,
          handshake: queue.handshake,
          utilization: queue.served,
          states: perShard[index],
        })),
      });
      connects = 0;
      failures = 0;
      wasted = 0;
      servedTotal = 0;
    }
  }
  return frames;
}

/** The engine's defaults (`RuntimeTuning`), used until the API answers. */
export const ENGINE_POLICY: RetryPolicy = { kind: "engine", baseMs: 5_000, maxMs: 300_000, maxRetries: 10 };

const BASE = {
  shards: 4,
  shardCapacity: 1_000,
  sendCost: 6,
  handshakeCost: 80,
  stallMs: 2_000,
  giveUpMs: 6_000,
  connectTimeoutMs: 4_000,
} as const;

/** Built-in scenarios, each a story with one trigger. */
export const SCENARIOS: readonly Scenario[] = [
  {
    ...BASE,
    id: "platform-outage",
    title: "A platform blips for 20 s",
    summary: "400 destinations on 4 shards run at 60% load. A platform carrying 40% of them drops every session for 20 s. Its destinations reconnect, and every reconnect costs a TLS and RTMP handshake on the shard.",
    durationMs: 150_000,
    destinations: 400,
    outageShare: 0.4,
    events: [{ atMs: 20_000, kind: "outage", untilMs: 40_000 }],
  },
  {
    ...BASE,
    id: "spike",
    title: "200 destinations go live at once",
    summary: "A show starts: 200 destinations are added in one second to a plant already carrying 300, each opening a connection to its platform.",
    durationMs: 120_000,
    destinations: 300,
    outageShare: 0,
    events: [{ atMs: 15_000, kind: "spike", add: 200, spreadMs: 1_000 }],
  },
  {
    ...BASE,
    id: "noisy-neighbour",
    title: "A shard loses half its CPU",
    summary: "A neighbour process takes half of one shard's CPU for 30 s. Its destinations stall, resync and reconnect, on that shard only.",
    durationMs: 120_000,
    destinations: 400,
    outageShare: 0,
    events: [{ atMs: 20_000, kind: "cpuCut", shard: 1, factor: 0.32, untilMs: 50_000 }],
  },
];
