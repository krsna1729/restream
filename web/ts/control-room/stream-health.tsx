// Stream health: what the publisher sends, judged in plain language
// (keyframes, bitrate, frame rate, loss) and charted from the pipeline's
// flight recorder, so an operator can tell "fix your encoder" from "our
// side is fine" without reading counters.

import { useMemo } from "react";

import { getPipelineProbe } from "../core/api.js";
import type { DeclaredEncoder, FlightSample } from "../core/api-types.js";
import { Kpi, KpiRow } from "../ui/kpi.js";
import { EmptyState, Panel } from "../ui/panel.js";
import { RawJson } from "../ui/raw-json.js";
import { TimeSeries } from "../ui/time-series.js";
import { StatusBadge } from "../ui/tone.js";
import type { Tone } from "../ui/tone.js";
import { formatBps } from "./format.js";
import { PictureAndSound } from "./picture-sound.js";
import { PipelineScope } from "./pipeline-scope.js";
import { EncoderMark } from "./encoder-mark.js";
import { declaredVerdicts, declaredWords, encoderIdentity } from "./encoder-model.js";
import { Freshness, useFlightRecorder, usePolled } from "./polled.js";
import type { LiveSnapshot } from "./store.js";
import { StreamTimeline } from "./stream-timeline.js";
import { KEYFRAME_LIMIT_SEC, KEYFRAME_TARGET_SEC, arrivingFps, bitrateWords, contributionTransport, overallSeverity, streamVerdicts } from "./stream-health-model.js";
import type { Severity } from "./stream-health-model.js";

const TONE: Record<Severity, Tone> = { ok: "ok", warn: "warn", bad: "bad" };
const STRIPE: Record<Severity, string> = { ok: "border-l-success", warn: "border-l-warning", bad: "border-l-error" };

export function StreamHealth(props: { live: LiveSnapshot; pipelineId: string | null }) {
  return (
    <PipelineScope live={props.live} workspace="health" pipelineId={props.pipelineId} verb="check">
      {(pipelineId) => <Health pipelineId={pipelineId} />}
    </PipelineScope>
  );
}

function Health(props: { pipelineId: string }) {
  const trace = useFlightRecorder(props.pipelineId);
  const samples = trace.value?.samples ?? [];
  // The same poll (and key) as Picture and sound: one request serves both.
  const probe = usePolled(["probe", props.pipelineId], () => getPipelineProbe(props.pipelineId), 10_000);
  const declared = probe.value?.encoder ?? null;
  const verdicts = useMemo(() => {
    const order: Record<Severity, number> = { bad: 0, warn: 1, ok: 2 };
    return [...streamVerdicts(samples), ...declaredVerdicts(declared, samples)].sort((a, b) => order[a.severity] - order[b.severity]);
  }, [samples, declared]);
  const last: FlightSample | undefined = samples[samples.length - 1];
  const ingest = last?.ingest ?? null;
  const fpsArriving = ingest ? arrivingFps(ingest) : null;
  const quality = bitrateWords(ingest?.bitrateBps, ingest?.height, fpsArriving);
  const transport = contributionTransport(ingest?.protocol);
  if (trace.value === null) {
    return <EmptyState title="Waiting for the flight recorder…" detail="The engine samples every pipeline every 2 s; the first sample is on its way." />;
  }
  const overall = overallSeverity(verdicts);
  return (
    <>
      <Panel
        id="health-verdicts"
        title={overall === "ok" ? "The publisher is healthy" : overall === "warn" ? "The publisher needs a look" : "The publisher is in trouble"}
        subtitle="judged over the last minute"
        stripe={overall}
        actions={<Freshness atMs={trace.atMs} staleAfterMs={trace.staleAfterMs} />}
      >
        {declared && <EncoderStrip declared={declared} />}
        <ul className="grid gap-3 md:grid-cols-2 xl:grid-cols-3" aria-label="Stream verdicts">
          {verdicts.map((verdict) => (
            <li key={verdict.key} className={`rounded-box border border-l-4 border-base-content/10 bg-base-100 p-3 ${STRIPE[verdict.severity]}`}>
              <div className="flex items-start justify-between gap-2">
                <b className="text-sm">{verdict.title}</b>
                <StatusBadge tone={TONE[verdict.severity]}>{verdict.severity === "ok" ? "ok" : verdict.severity === "warn" ? "check" : "fix"}</StatusBadge>
              </div>
              <p className="mt-1 text-sm text-base-content/70">{verdict.detail}</p>
            </li>
          ))}
        </ul>
      </Panel>
      <KpiRow max={6}>
        <Kpi
          label="Video"
          value={ingest?.height ? `${ingest.height}p` : "--"}
          foot={ingest ? `${(ingest.videoCodec ?? "?").toUpperCase()} · ${ingest.width ?? "?"}×${ingest.height ?? "?"} · ${ingest.protocol.toUpperCase()}` : "no source"}
        />
        <Kpi
          label="Bitrate"
          value={formatBps(ingest?.bitrateBps)}
          tone={quality?.severity === "warn" ? "warn" : undefined}
          foot={quality?.words ?? (ingest ? `${Math.round(ingest.packetsPerSec)} packets/s` : "--")}
        />
        <Kpi
          label="Frame rate"
          value={fpsArriving ? `${fpsArriving.toFixed(fpsArriving % 1 ? 1 : 0)} fps` : "--"}
          foot={ingest?.timeline && ingest.fps ? `arriving · stream says ${Number(ingest.fps.toFixed(2))}` : undefined}
        />
        <Kpi
          label="Keyframe interval"
          value={ingest?.gopSec ? `${ingest.gopSec.toFixed(1)} s` : "--"}
          tone={ingest?.maxGopSec ? (ingest.maxGopSec > KEYFRAME_LIMIT_SEC ? "bad" : ingest.maxGopSec > KEYFRAME_TARGET_SEC * 1.05 ? "warn" : undefined) : undefined}
          foot={ingest?.maxGopSec ? `longest ${ingest.maxGopSec.toFixed(1)} s · target ${KEYFRAME_TARGET_SEC} s` : `target ${KEYFRAME_TARGET_SEC} s`}
        />
        <Kpi label="Round trip" value={ingest?.rttMs ? `${Math.round(ingest.rttMs)} ms` : "--"} foot={transport ?? "no network link"} />
        <Kpi
          label="Loss"
          value={ingest?.lossPerSec === null || ingest?.lossPerSec === undefined ? "--" : `${ingest.lossPerSec.toFixed(1)}/s`}
          foot={ingest?.retransPerSec ? `${ingest.retransPerSec.toFixed(1)} retransmits/s` : transport ? "not reported by this protocol" : "no network link"}
        />
      </KpiRow>
      <div className="grid gap-4 xl:grid-cols-2">
        <Panel id="health-bitrate" title="Bitrate" subtitle="over each 2 s interval">
          <TimeSeries
            samples={samples}
            label="Ingest bitrate over time"
            series={[{ id: "bitrate", label: "Bitrate", pick: (sample) => sample.ingest?.bitrateBps ?? null, color: "text-primary", area: true, format: formatBps }]}
          />
        </Panel>
        <Panel id="health-keyframes" title="Keyframe interval" subtitle={`seconds · platforms want ${KEYFRAME_TARGET_SEC}`}>
          <TimeSeries
            samples={samples}
            label="Keyframe interval over time"
            thresholds={[
              { value: KEYFRAME_TARGET_SEC, label: `${KEYFRAME_TARGET_SEC} s target`, tone: "ok" },
              { value: KEYFRAME_LIMIT_SEC, label: `${KEYFRAME_LIMIT_SEC} s limit`, tone: "bad" },
            ]}
            series={[
              { id: "gop", label: "Mean", pick: (sample) => sample.ingest?.gopSec ?? null, color: "text-accent", format: (value) => `${value.toFixed(2)} s` },
              { id: "max", label: "Longest", pick: (sample) => sample.ingest?.maxGopSec ?? null, color: "text-warning", dashed: true, format: (value) => `${value.toFixed(2)} s` },
            ]}
          />
        </Panel>
        <Panel id="health-fps" title="Frame rate">
          <TimeSeries
            samples={samples}
            label="Frame rate over time"
            series={[{ id: "fps", label: "fps", pick: (sample) => (sample.ingest ? arrivingFps(sample.ingest) : null), color: "text-secondary", area: true, format: (value) => `${value.toFixed(1)} fps` }]}
          />
        </Panel>
        <Panel id="health-link" title="Contribution link" subtitle="round trip and loss">
          {ingest && !transport ? (
            <EmptyState
              title="No contribution link"
              detail="The engine reads this source itself; nothing crosses a network, so there is no round trip or loss to chart."
            />
          ) : (
            <TimeSeries
              samples={samples}
              label="Round trip and loss over time"
              series={[
                { id: "rtt", label: "RTT ms", pick: (sample) => sample.ingest?.rttMs ?? null, color: "text-info", format: (value) => `${value.toFixed(0)} ms` },
                { id: "loss", label: "Loss/s", pick: (sample) => sample.ingest?.lossPerSec ?? null, color: "text-error", format: (value) => value.toFixed(1) },
              ]}
            />
          )}
        </Panel>
      </div>
      <StreamTimeline timeline={ingest?.timeline ?? null} decoders={last?.decoders ?? []} />
      <PictureAndSound pipelineId={props.pipelineId} samples={samples} />
      <RawJson value={last ?? null} label="newest flight sample" />
    </>
  );
}

/** Who is publishing: the encoder's name and mark, and what it says it is
 * set to send (RTMP `onMetaData`). */
function EncoderStrip(props: { declared: DeclaredEncoder }) {
  const identity = props.declared.encoder ? encoderIdentity(props.declared.encoder) : null;
  const settings = declaredWords(props.declared);
  return (
    <div className="mb-3 flex items-center gap-3" aria-label="Encoder">
      {identity && <EncoderMark identity={identity} size={32} />}
      <div className="min-w-0 text-sm">
        <div className="truncate font-medium" title={identity?.raw}>
          {identity ? `${identity.name}${identity.version ? ` ${identity.version}` : ""}` : "Encoder"}
        </div>
        {settings && <div className="truncate text-base-content/70">set to {settings}</div>}
      </div>
    </div>
  );
}
