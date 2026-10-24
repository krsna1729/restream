import { useEffect, useState } from "react";

import {
  getEngineSbom,
  getEngineSbomEndpoint,
  getEngineStatus,
} from "../core/api.js";
import type { SrtIngressOwnerHealth } from "../types.js";
import { formatBytes, formatPercent } from "./format.js";
import { Freshness, useEngineHealth } from "./polled.js";
import type { LiveSnapshot } from "./store.js";
import {
  executionTopology,
  hostReadiness,
  kernelCapabilities,
  overallReadiness,
  ownerAttention,
} from "./system-model.js";
import type { CheckStatus, EngineStatus } from "./system-model.js";
import { Kpi, KeyValue, KpiRow } from "../ui/kpi.js";
import { PageHeader } from "../ui/page-header.js";
import { Panel } from "../ui/panel.js";
import { StatusBadge } from "../ui/tone.js";
import type { Tone } from "../ui/tone.js";

const CHECK_TONE: Record<CheckStatus, Tone> = {
  ok: "ok",
  warning: "warn",
  unknown: "neutral",
};

/** Whether this host suits the engine, what runs, and what this build is.
 * Counts are the engine's report; a class it reports only as configured
 * says so. */
export function System({ live }: { live: LiveSnapshot }) {
  const health = useEngineHealth();
  const [status, setStatus] = useState<EngineStatus | null>(null);
  useEffect(() => {
    void getEngineStatus<EngineStatus>().then(setStatus);
  }, []);
  if (health.value === null) {
    return <p className="text-sm text-base-content/70">Loading engine health…</p>;
  }
  const rows = hostReadiness(health.value);
  const overall = overallReadiness(rows);
  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        eyebrow="System"
        title={overall === "ok" ? "Host ready" : overall === "warning" ? "A host check needs attention" : "Host checks not reported"}
        tone={overall === "ok" ? "ok" : overall === "warning" ? "warn" : undefined}
        subtitle="Host readiness, kernel capabilities, execution classes, SRT ingress counters and this build."
        actions={<Freshness atMs={health.atMs} staleAfterMs={health.staleAfterMs} />}
      />
      <div className="grid gap-4 lg:grid-cols-2">
        <Panel id="system-host" title="Host readiness">
          <div className="flex flex-col gap-3">
            <ul className="space-y-2" aria-label="Host checks">
              {rows.map((row) => (
                <li key={row.key} className="grid grid-cols-[1fr_auto] gap-x-2 text-sm">
                  <span className="font-medium">{row.label}</span>
                  <span className="flex items-center gap-2 tabular-nums">
                    {row.value}
                    <StatusBadge tone={CHECK_TONE[row.status]} dot>
                      {row.status}
                    </StatusBadge>
                  </span>
                  <span className="col-span-2 text-xs text-base-content/70">{row.detail}</span>
                </li>
              ))}
            </ul>
            <ul className="flex flex-wrap gap-2 text-sm" aria-label="Kernel capabilities">
              {kernelCapabilities(live.metrics).map((cap) => (
                <li key={cap.label} className="max-w-full">
                  <StatusBadge tone={cap.available === null ? "neutral" : cap.available ? "ok" : "warn"}>
                    {cap.label} · {cap.available === null ? "unknown" : cap.available ? "available" : "unavailable"} · {cap.detail}
                  </StatusBadge>
                </li>
              ))}
            </ul>
          </div>
        </Panel>
        <Panel id="system-topology" title="Execution topology" subtitle="reported by the engine">
          <div className="flex flex-col gap-3">
            <KpiRow max={3}>
              {executionTopology(health.value, live.metrics).map((cls) => (
                <Kpi key={cls.name} label={cls.name} value={cls.count ?? "--"} foot={cls.unit} />
              ))}
            </KpiRow>
            <Process live={live} />
          </div>
        </Panel>
        <SrtOwners
          owner={health.value.srtListener?.ingressOwner ?? null}
          restarts={health.value.srtListener?.restarts ?? 0}
        />
        <Provenance status={status} containedPanics={health.value.containedPanics ?? 0} />
      </div>
    </div>
  );
}

function Process({ live }: { live: LiveSnapshot }) {
  const engine = live.metrics?.engine;
  if (!engine) return null;
  const sampled = engine.cpuSampleReady !== false;
  return (
    <KeyValue
      rows={[
        ["Restream CPU", sampled ? formatPercent(engine.restreamCpuPercent) : "sampling…"],
        ["FFmpeg CPU", sampled ? formatPercent(engine.externalFfmpegCpuPercent) : "sampling…"],
        ["Memory", `${formatBytes(engine.restreamMemoryBytes)} restream · ${formatBytes(engine.externalFfmpegMemoryBytes)} FFmpeg`],
      ]}
    />
  );
}

function SrtOwners(props: { owner: SrtIngressOwnerHealth | null; restarts: number }) {
  const { owner } = props;
  return (
    <Panel id="system-owners" title="SRT ingress owners" subtitle="summed over every owner; counters since the engine started">
      <div className="flex flex-col gap-3">
        {owner === null ? (
          <p className="text-sm text-base-content/70">The SRT listener reports no owner.</p>
        ) : (
          <>
            <div className="grid gap-x-6 gap-y-1.5 sm:grid-cols-2">
              <KeyValue
                rows={[
                  ["Peers", owner.peers],
                  ["RX ring depth", owner.rxRingDepth],
                  ["TX in flight", `${owner.txInFlight} / ${owner.txCapacity}`],
                  ["Listener restarts", props.restarts],
                ]}
              />
              <KeyValue
                rows={[
                  ["Budget exhausted", owner.budgetExhausted],
                  ["Slow passes > 5 ms", owner.mediaSlowPasses5ms],
                  ["Longest pass", `${owner.mediaPassMaxUs} µs`],
                  ["Telemetry dropped", owner.telemetryDropped],
                ]}
              />
            </div>
            <Attention findings={ownerAttention(owner)} />
          </>
        )}
      </div>
    </Panel>
  );
}

function Attention({ findings }: { findings: readonly string[] }) {
  if (findings.length === 0) {
    return (
      <p className="text-sm text-success">
        No dropped or late work since the engine started.
      </p>
    );
  }
  return (
    <div className="text-sm text-warning">
      <p>Since the engine started:</p>
      <ul className="list-inside list-disc" aria-label="Owner findings">
        {findings.map((finding) => (
          <li key={finding}>{finding}</li>
        ))}
      </ul>
    </div>
  );
}

function Provenance(props: { status: EngineStatus | null; containedPanics: number }) {
  const { status } = props;
  if (status === null) {
    return <p className="text-sm text-base-content/70">Loading build information…</p>;
  }
  const libs = status.nativeLibraries ?? {};
  return (
    <Panel id="system-build" title="Build and provenance">
      <div className="flex flex-col gap-3">
        <KeyValue
          rows={[
            ["Restream", status.restream?.version ?? "--"],
            ["Commit", <span key="commit" className="font-mono text-xs break-all">{status.restream?.commit ?? "--"}</span>],
            ["Built", status.restream?.buildTimestamp ?? "--"],
            ["Rust", `${status.toolchain?.rustc ?? "--"} · ${status.toolchain?.target ?? "--"}`],
            ...Object.entries(libs).map(([name, lib]) => [name, `${lib?.version ?? "--"}${lib?.license ? ` · ${lib.license}` : ""}`] as const),
            ["Host", `${status.os?.hostname ?? "--"} · kernel ${status.os?.kernelVersion ?? "--"} · ${status.os?.cpu?.logicalCpus ?? "--"} CPUs`],
            ["Contained panics", <span key="panics" className={props.containedPanics > 0 ? "text-warning" : undefined}>{props.containedPanics}</span>],
          ]}
        />
        <div className="flex flex-wrap items-center gap-2">
          <button
            type="button"
            className="btn btn-sm"
            onClick={async () => {
              const sbom = await getEngineSbom(getEngineSbomEndpoint(status));
              if (sbom !== null) download("restream-sbom.cdx.json", sbom);
            }}
          >
            Download SBOM
          </button>
          <span className="text-xs text-base-content/70">
            {status.sbom?.componentCount ?? "--"} components
            {status.sbom?.licensesIncluded ? " · licenses included" : ""}
          </span>
        </div>
      </div>
    </Panel>
  );
}

function download(filename: string, data: unknown): void {
  const url = URL.createObjectURL(
    new Blob([JSON.stringify(data, null, 2)], { type: "application/json" }),
  );
  const link = document.createElement("a");
  link.href = url;
  link.download = filename;
  link.click();
  URL.revokeObjectURL(url);
}
