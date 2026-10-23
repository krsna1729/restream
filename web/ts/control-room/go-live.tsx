// Operate's "Going live" panel: a short checklist (source, keyframes,
// destinations, live, headroom) and how to point an encoder at this
// pipeline. When every check passes it folds to one line.

import { useState } from "react";

import { getFlightRecorder } from "../core/api.js";
import type { PipelineInput } from "../types.js";
import { CopyButton } from "../ui/controls.js";
import { Panel } from "../ui/panel.js";
import { StatusDot } from "../ui/tone.js";
import type { Tone } from "../ui/tone.js";
import { ENCODER_SETTINGS, encoderFields, goLiveChecks } from "./go-live-model.js";
import type { CheckState } from "./go-live-model.js";
import { isDelivering } from "./fleet-model.js";
import { usePolled } from "./polled.js";
import type { LiveSnapshot } from "./store.js";

const TONE: Record<CheckState, Tone> = { ok: "ok", warn: "warn", todo: "neutral" };
const SAFE_UTILIZATION = 0.8;

export function GoLive(props: { live: LiveSnapshot; pipelineId: string; inputs: readonly PipelineInput[] | null }) {
  const { state } = props.live;
  const pipeline = state?.pipelines.find((candidate) => candidate.id === props.pipelineId) ?? null;
  const outputs = (state?.outputs ?? []).filter((output) => output.pipelineId === props.pipelineId);
  // The last minute of the flight recorder says how far apart keyframes are.
  const trace = usePolled(
    ["go-live-trace", props.pipelineId],
    () => getFlightRecorder(props.pipelineId, new Date(Date.now() - 60_000), { points: 30 }),
    10_000,
  );
  const gops = (trace.value?.samples ?? []).flatMap((sample) => (sample.ingest?.maxGopSec ? [sample.ingest.maxGopSec] : []));
  const wanted = outputs.filter((output) => output.desired === "running");
  const checks = goLiveChecks({
    sourceOn: pipeline?.ingest != null,
    maxGopSec: gops.length ? Math.max(...gops) : null,
    destinations: outputs.length,
    wanted: wanted.length,
    delivering: wanted.filter(isDelivering).length,
    utilization: props.live.metrics?.capacity?.projectedUtilization ?? null,
    safeUtilization: SAFE_UTILIZATION,
  });
  const passed = checks.filter((check) => check.state === "ok").length;
  const ready = passed === checks.length;
  const [open, setOpen] = useState<boolean | null>(null);
  const expanded = open ?? !ready;
  const keyed = (props.inputs ?? []).filter((input) => input.kind !== "hlsPull" && input.ingestUrls);
  return (
    <Panel
      id="go-live"
      title={ready ? "On air and healthy" : "Going live"}
      subtitle={`${passed} of ${checks.length} checks pass`}
      stripe={ready ? "ok" : checks.some((check) => check.state === "warn") ? "warn" : undefined}
      actions={
        <button type="button" className="btn btn-ghost btn-sm" aria-expanded={expanded} onClick={() => setOpen(!expanded)}>
          {expanded ? "Hide" : "Show"} checks
        </button>
      }
    >
      {expanded ? (
        <div className="grid gap-4 lg:grid-cols-2">
          <ol className="flex flex-col gap-2" aria-label="Go-live checks">
            {checks.map((check) => (
              <li key={check.key} className="flex gap-2.5 text-sm">
                <StatusDot tone={TONE[check.state]} className="mt-1.5" />
                <span className="min-w-0">
                  <span className="font-medium">{check.title}</span>
                  <span className="block text-xs text-base-content/70">{check.detail}</span>
                </span>
              </li>
            ))}
          </ol>
          <EncoderSetup inputs={keyed} />
        </div>
      ) : (
        <p className="text-sm text-base-content/70">{checks.map((check) => check.title).join(" · ")}</p>
      )}
    </Panel>
  );
}

/** Where an encoder (OBS: Settings → Stream → Service "Custom") sends. */
function EncoderSetup(props: { inputs: readonly PipelineInput[] }) {
  const [inputId, setInputId] = useState(props.inputs[0]?.id ?? "");
  const [reveal, setReveal] = useState(false);
  const input = props.inputs.find((candidate) => candidate.id === inputId) ?? props.inputs[0];
  if (!input?.ingestUrls) {
    return <p className="text-sm text-base-content/70">This pipeline takes no pushes: its sources pull or play files.</p>;
  }
  const rtmp = input.ingestUrls.rtmp ? encoderFields(input.ingestUrls.rtmp) : null;
  // The key rides in every URL (RTMP path, SRT streamid): hidden in all
  // of them until asked for; Copy always copies the real value.
  const secret = rtmp?.key ?? input.streamKey;
  const masked = (text: string) => (reveal || !secret ? text : text.split(secret).join("•".repeat(Math.min(16, secret.length))));
  return (
    <section aria-label="Set up your encoder" className="flex min-w-0 flex-col gap-2 rounded-box border border-base-content/10 bg-base-100 p-3 text-sm">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h4 className="font-semibold">Set up your encoder</h4>
        {props.inputs.length > 1 && (
          <select className="select select-sm w-40" aria-label="Encoder input" value={input.id} onChange={(event) => setInputId(event.target.value)}>
            {props.inputs.map((candidate) => (
              <option key={candidate.id} value={candidate.id}>
                {candidate.label}
              </option>
            ))}
          </select>
        )}
      </div>
      <p className="text-xs text-base-content/70">In OBS: Settings → Stream → Service “Custom…”, then paste these.</p>
      <dl className="grid grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-x-3 gap-y-1">
        {rtmp && (
          <>
            <dt className="text-xs text-base-content/70">Server</dt>
            <dd className="truncate font-mono text-xs" title={masked(rtmp.server)}>
              {masked(rtmp.server)}
            </dd>
            <dd>
              <CopyButton text={rtmp.server} label="Copy" />
            </dd>
            {rtmp.key && (
              <>
                <dt className="text-xs text-base-content/70">Stream key</dt>
                <dd className="truncate font-mono text-xs">
                  {masked(rtmp.key)}{" "}
                  <button type="button" className="link link-hover touch-36 inline-flex items-center text-xs" onClick={() => setReveal(!reveal)}>
                    {reveal ? "hide" : "show"}
                  </button>
                </dd>
                <dd>
                  <CopyButton text={rtmp.key} label="Copy" />
                </dd>
              </>
            )}
          </>
        )}
        {input.ingestUrls.srt && (
          <>
            <dt className="text-xs text-base-content/70">SRT</dt>
            <dd className="truncate font-mono text-xs" title={masked(input.ingestUrls.srt)}>
              {masked(input.ingestUrls.srt)}
            </dd>
            <dd>
              <CopyButton text={input.ingestUrls.srt} label="Copy" />
            </dd>
          </>
        )}
      </dl>
      <details className="text-xs">
        <summary className="cursor-pointer text-base-content/70">Output settings platforms ask for</summary>
        <dl className="mt-1 grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-0.5">
          {ENCODER_SETTINGS.map(([label, value]) => (
            <div key={label} className="contents">
              <dt className="text-base-content/70">{label}</dt>
              <dd>{value}</dd>
            </div>
          ))}
        </dl>
      </details>
    </section>
  );
}
