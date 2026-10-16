import { useEffect, useState } from "react";

import {
  getEngineSbom,
  getEngineSbomEndpoint,
  getEngineStatus,
} from "../core/api.js";
import type { HealthData, SrtIngressOwnerHealth } from "../types.js";
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

const CHECK_BADGE: Record<CheckStatus, string> = {
  ok: "badge-success",
  warning: "badge-warning",
  unknown: "badge-ghost",
};

const CHECK_LABEL: Record<CheckStatus, string> = {
  ok: "host checks pass",
  warning: "a host check fails",
  unknown: "a host check is unknown",
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
    return <p className="opacity-70">Loading engine health…</p>;
  }
  const rows = hostReadiness(health.value);
  const overall = overallReadiness(rows);
  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-3">
        <h2 className="text-xl font-semibold">System</h2>
        <span className={`badge ${CHECK_BADGE[overall]}`}>{CHECK_LABEL[overall]}</span>
        <Freshness atMs={health.atMs} staleAfterMs={health.staleAfterMs} />
      </div>
      <div className="grid gap-4 lg:grid-cols-2">
        <section className="card bg-base-200" aria-labelledby="system-host">
          <div className="card-body gap-2 p-4">
            <h3 id="system-host" className="card-title text-base">
              Host readiness
            </h3>
            <ul className="space-y-2" aria-label="Host checks">
              {rows.map((row) => (
                <li key={row.key} className="grid grid-cols-[1fr_auto] gap-x-2 text-sm">
                  <span className="font-medium">{row.label}</span>
                  <span className="flex items-center gap-2 tabular-nums">
                    {row.value}
                    <span className={`badge badge-sm ${CHECK_BADGE[row.status]}`}>
                      {row.status}
                    </span>
                  </span>
                  <span className="col-span-2 text-xs opacity-70">{row.detail}</span>
                </li>
              ))}
            </ul>
            <ul className="flex flex-wrap gap-2 text-sm" aria-label="Kernel capabilities">
              {kernelCapabilities(live.metrics).map((cap) => (
                <li
                  key={cap.label}
                  className={`badge ${
                    cap.available === null
                      ? "badge-ghost"
                      : cap.available
                        ? "badge-success"
                        : "badge-warning"
                  }`}
                >
                  {cap.label} · {cap.available === null ? "unknown" : cap.available ? "available" : "unavailable"} · {cap.detail}
                </li>
              ))}
            </ul>
          </div>
        </section>
        <section className="card bg-base-200" aria-labelledby="system-topology">
          <div className="card-body gap-3 p-4">
            <h3 id="system-topology" className="card-title text-base">
              Execution topology
              <span className="text-sm font-normal opacity-70">reported by the engine</span>
            </h3>
            <dl className="grid grid-cols-2 gap-2 sm:grid-cols-3">
              {executionTopology(health.value, live.metrics).map((cls) => (
                <div key={cls.name} className="rounded bg-base-100 p-2">
                  <dt className="text-xs opacity-70">{cls.name}</dt>
                  <dd className="text-2xl tabular-nums">{cls.count ?? "--"}</dd>
                  <dd className="text-xs opacity-70">{cls.unit}</dd>
                </div>
              ))}
            </dl>
            <Process live={live} />
          </div>
        </section>
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
    <dl className="grid grid-cols-[8rem_1fr] gap-1 text-sm">
      <dt className="opacity-70">Restream CPU</dt>
      <dd className="tabular-nums">
        {sampled ? formatPercent(engine.restreamCpuPercent) : "sampling…"}
      </dd>
      <dt className="opacity-70">FFmpeg CPU</dt>
      <dd className="tabular-nums">
        {sampled ? formatPercent(engine.externalFfmpegCpuPercent) : "sampling…"}
      </dd>
      <dt className="opacity-70">Memory</dt>
      <dd className="tabular-nums">
        {formatBytes(engine.restreamMemoryBytes)} restream ·{" "}
        {formatBytes(engine.externalFfmpegMemoryBytes)} FFmpeg
      </dd>
    </dl>
  );
}

function SrtOwners(props: { owner: SrtIngressOwnerHealth | null; restarts: number }) {
  const { owner } = props;
  return (
    <section className="card bg-base-200" aria-labelledby="system-owners">
      <div className="card-body gap-3 p-4">
        <h3 id="system-owners" className="card-title text-base">
          SRT ingress owners
          <span className="text-sm font-normal opacity-70">
            summed over every owner; counters since the engine started
          </span>
        </h3>
        {owner === null ? (
          <p className="text-sm opacity-70">The SRT listener reports no owner.</p>
        ) : (
          <>
            <dl className="grid grid-cols-2 gap-2 text-sm sm:grid-cols-4">
              {[
                ["Peers", owner.peers],
                ["RX ring depth", owner.rxRingDepth],
                ["TX in flight", `${owner.txInFlight} / ${owner.txCapacity}`],
                ["Listener restarts", props.restarts],
                ["Budget exhausted", owner.budgetExhausted],
                ["Slow passes > 5 ms", owner.mediaSlowPasses5ms],
                ["Longest pass", `${owner.mediaPassMaxUs} µs`],
                ["Telemetry dropped", owner.telemetryDropped],
              ].map(([label, value]) => (
                <div key={label}>
                  <dt className="text-xs opacity-70">{label}</dt>
                  <dd className="tabular-nums">{value}</dd>
                </div>
              ))}
            </dl>
            <Attention findings={ownerAttention(owner)} />
          </>
        )}
      </div>
    </section>
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
    return <p className="opacity-70">Loading build information…</p>;
  }
  const libs = status.nativeLibraries ?? {};
  return (
    <section className="card bg-base-200" aria-labelledby="system-build">
      <div className="card-body gap-3 p-4">
        <h3 id="system-build" className="card-title text-base">
          Build and provenance
        </h3>
        <dl className="grid grid-cols-[8rem_1fr] gap-1 text-sm">
          <dt className="opacity-70">Restream</dt>
          <dd>{status.restream?.version ?? "--"}</dd>
          <dt className="opacity-70">Commit</dt>
          <dd className="break-all font-mono text-xs">{status.restream?.commit ?? "--"}</dd>
          <dt className="opacity-70">Built</dt>
          <dd>{status.restream?.buildTimestamp ?? "--"}</dd>
          <dt className="opacity-70">Rust</dt>
          <dd>
            {status.toolchain?.rustc ?? "--"} · {status.toolchain?.target ?? "--"}
          </dd>
          {Object.entries(libs).map(([name, lib]) => (
            <div key={name} className="contents">
              <dt className="opacity-70">{name}</dt>
              <dd>
                {lib?.version ?? "--"}
                {lib?.license ? ` · ${lib.license}` : ""}
              </dd>
            </div>
          ))}
          <dt className="opacity-70">Host</dt>
          <dd>
            {status.os?.hostname ?? "--"} · kernel {status.os?.kernelVersion ?? "--"} ·{" "}
            {status.os?.cpu?.logicalCpus ?? "--"} CPUs
          </dd>
          <dt className="opacity-70">Contained panics</dt>
          <dd className={props.containedPanics > 0 ? "text-warning" : undefined}>
            {props.containedPanics}
          </dd>
        </dl>
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
          <span className="text-xs opacity-70">
            {status.sbom?.componentCount ?? "--"} components
            {status.sbom?.licensesIncluded ? " · licenses included" : ""}
          </span>
        </div>
      </div>
    </section>
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
