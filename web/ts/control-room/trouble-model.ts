// Trouble in plain words: what went wrong, and what to do about it. The
// engine's own text (socket errors, protocol replies) stays available as
// `raw` for whoever needs it; people read the summary and the fix first.

import type { Cause, OutputState } from "../core/generated/runtime-state.js";

export interface Explanation {
  /** One short sentence: what is wrong. */
  readonly summary: string;
  /** What to do, when there is something to do. */
  readonly fix: string | null;
  /** The engine's own words, when they differ from the summary. */
  readonly raw: string | null;
}

/** Known failure texts, most specific first. */
const KNOWN: ReadonlyArray<readonly [RegExp, string, string]> = [
  [
    /already (publishing|in use|streaming)|stream ?key (is )?in use|NetStream\.Publish\.BadName|duplicate/i,
    "Another encoder is already streaming with this key",
    "Stop the other encoder, or give this destination its own key.",
  ],
  [
    /unauthori[sz]ed|forbidden|\b40[13]\b|auth(entication)? failed|invalid (stream )?key|bad (stream )?key|publish.*(denied|rejected)|NetConnection\.Connect\.Rejected/i,
    "The platform rejected the stream key",
    "Copy the stream key again from the platform's dashboard.",
  ],
  [
    /passphrase|encrypt|bad ?secret|srt.*reject|reject(ed)? .*srt/i,
    "The SRT server refused the stream",
    "Check the stream id and passphrase the server expects.",
  ],
  [
    /certificate|tls|ssl|handshake/i,
    "The secure connection could not be set up",
    "Use rtmps:// with the platform's TLS port (usually 443), and check the host name.",
  ],
  [
    /failed to lookup|name resolution|no such host|nodename|dns|os error -?2\b|unknown host/i,
    "The server's name could not be found",
    "Check the spelling of the host in the URL.",
  ],
  [
    /connection refused|os error 111\b/i,
    "The server refused the connection",
    "Check the URL and port, and that the platform is expecting a stream.",
  ],
  [
    /network is unreachable|no route|os error 101\b|os error 113\b/i,
    "This machine cannot reach the server",
    "Check this host's network and firewall.",
  ],
  [
    /timed? ?out|os error 110\b|deadline/i,
    "The server did not answer in time",
    "Check the server is reachable from this host; a firewall may be dropping the traffic.",
  ],
  [
    /broken pipe|connection reset|os error (32|104)\b|unexpected eof|closed by peer|end of stream/i,
    "The server closed the connection",
    "Usually the platform ended the session; it reconnects on its own. If it repeats, check the platform's stream settings.",
  ],
];

/** A failure text in plain words, or null when it is not one we know. */
export function explainError(raw: string): { summary: string; fix: string } | null {
  for (const [pattern, summary, fix] of KNOWN) if (pattern.test(raw)) return { summary, fix };
  return null;
}

/** A shared stage key (`pipeline:kind:name`) as its readable last part. */
function stageName(stage: string): string {
  return stage.split(":").filter(Boolean).pop() ?? stage;
}

/** A diagnosed cause as people say it, with the fix. */
export function explainCause(cause: Cause): Explanation {
  switch (cause.code) {
    case "noPublisher":
      return { summary: "No publisher connected", fix: "Start the encoder, or check it uses this pipeline's stream key.", raw: null };
    case "stageNotProducing":
      return {
        summary: `Waiting on the ${stageName(cause.stage)} stage`,
        fix: "It starts producing once the source is on air; if the source is fine, see Inspect.",
        raw: `${cause.stage} (${cause.phase})`,
      };
    case "shardPanicked":
      return { summary: "Its sending thread crashed", fix: "The engine replaces it; the destination reconnects on its own.", raw: `egress shard ${cause.shard} panicked` };
    case "shardStalled":
      return { summary: "Its sending thread is stuck", fix: "Restart the destination. If many are stuck, see Inspect, Egress shards.", raw: `egress shard ${cause.shard} stalled` };
    case "shardStopped":
      return { summary: "Its sending thread stopped", fix: "Restart the destination.", raw: `egress shard ${cause.shard} stopped` };
    case "capacityRefused":
      return { summary: "Refused: this host is at its sending capacity", fix: "Stop other destinations, or run this one on a host with more CPUs.", raw: cause.error };
    case "retriesExhausted":
      return { summary: "Gave up after repeated failures", fix: "Fix the cause in its log, then start it again.", raw: null };
    case "retryBudgetNearlySpent":
      return {
        summary: `Retrying (${cause.attempts} of ${cause.max})`,
        fix: `It gives up after attempt ${cause.max}; check the platform before then.`,
        raw: null,
      };
    case "rejected": {
      const known = explainError(cause.error);
      const raw = cause.phase ? `${cause.phase}: ${cause.error}` : cause.error;
      return known ? { ...known, raw } : { summary: "The server rejected the stream", fix: null, raw };
    }
    case "backpressured":
      return cause.stalled
        ? { summary: "Stuck: the server is not accepting data", fix: "Check the platform's ingest health and this host's upload bandwidth.", raw: "backpressured and stalled" }
        : { summary: "Sending slower than the stream", fix: "Check this host's upload bandwidth.", raw: "backpressured" };
    case "noProgress":
      return { summary: "Connected, but nothing is being sent", fix: "Restart the destination.", raw: null };
  }
}

/** What is wrong with one destination now, if anything. */
export function outputTrouble(output: Pick<OutputState, "diagnosis" | "evidence" | "backpressure">): Explanation | null {
  if (output.diagnosis) return explainCause(output.diagnosis.cause);
  if (output.evidence) {
    const raw = output.evidence.phase ? `${output.evidence.phase}: ${output.evidence.error}` : output.evidence.error;
    const known = explainError(output.evidence.error);
    return known ? { ...known, raw } : { summary: "The last attempt failed", fix: null, raw };
  }
  if (output.backpressure) return { summary: "Held back by a slow consumer", fix: "Check this host's upload bandwidth.", raw: output.backpressure };
  return null;
}

/** A tooltip with the fix and the engine's own words. */
export function troubleTitle(trouble: Explanation): string {
  return [trouble.summary, trouble.fix, trouble.raw && `Engine: ${trouble.raw}`].filter(Boolean).join("\n");
}
