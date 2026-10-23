// What the System workspace shows: whether the host is set up for this
// engine, which execution classes run and how many of each, how the SRT
// ingress Owners are doing, and what this build is.

import type {
  HealthData,
  HostSettingRow,
  SrtIngressOwnerHealth,
  SystemMetrics,
} from "../types.js";
import { formatBytes } from "./format.js";

/** The subset of `GET /api/v1/engine` the page shows. */
export interface EngineStatus {
  restream?: { version?: string; commit?: string; buildTimestamp?: string };
  toolchain?: { rustc?: string; target?: string };
  nativeLibraries?: Record<string, { version?: string; license?: string } | undefined>;
  sbom?: { endpoint?: string; componentCount?: number; licensesIncluded?: boolean };
  os?: {
    hostname?: string;
    kernelVersion?: string | null;
    cpu?: { modelName?: string | null; logicalCpus?: number };
  };
}

export type CheckStatus = "ok" | "warning" | "unknown";

export interface ReadinessRow {
  readonly key: string;
  readonly label: string;
  readonly value: string;
  readonly status: CheckStatus;
  readonly detail: string;
}

function checkStatus(value: string | undefined): CheckStatus {
  return value === "ok" || value === "warning" ? value : "unknown";
}

function settingValue(row: HostSettingRow): string {
  const { current, unit } = row;
  if (current === null || current === undefined) return "--";
  if (typeof current === "string") return current;
  if (unit === "bytes") return formatBytes(current);
  return `${current.toLocaleString()}${unit ? ` ${unit}` : ""}`;
}

/** The host checks, a warning first; each says what it measured. */
export function hostReadiness(health: HealthData): readonly ReadinessRow[] {
  const order: Record<CheckStatus, number> = { warning: 0, unknown: 1, ok: 2 };
  return (health.hostSettings ?? [])
    .map((row) => ({
      key: row.key,
      label: row.label,
      value: settingValue(row),
      status: checkStatus(row.status),
      detail: row.detail ?? "",
    }))
    .sort((a, b) => order[a.status] - order[b.status]);
}

/** The worst check: one warning makes the host not ready. */
export function overallReadiness(rows: readonly ReadinessRow[]): CheckStatus {
  if (rows.some((row) => row.status === "warning")) return "warning";
  if (rows.some((row) => row.status === "unknown")) return "unknown";
  return "ok";
}

export interface ExecutionClass {
  readonly name: string;
  /** How many there are; null when the engine did not report it. */
  readonly count: number | null;
  readonly unit: string;
}

/** Each execution class and how many of it there are: running where the
 * engine reports a live count, configured where it reports only that. */
export function executionTopology(
  health: HealthData,
  metrics: SystemMetrics | null,
): readonly ExecutionClass[] {
  const tokio = health.hostSettings?.find(
    (row) => row.key === "runtime.tokio.worker_threads",
  )?.current;
  const media = metrics?.mediaExecutor;
  return [
    {
      name: "Control",
      count: typeof tokio === "number" ? tokio : null,
      unit: "Tokio workers, configured",
    },
    {
      name: "Media",
      // The media runtime starts with its first service; until then none run.
      count: media?.workers?.length ?? null,
      unit: `media workers running of ${media?.configuredWorkers ?? "--"}`,
    },
    {
      name: "RTMP ingress",
      count: health.rtmpListener?.ingressOwners ?? null,
      unit: "Compio owners",
    },
    {
      name: "SRT ingress",
      count: health.srtListener?.ingressOwner?.owners ?? null,
      unit: "Compio owners",
    },
    {
      name: "Egress fabric",
      count: health.egressFabricShards?.length ?? null,
      unit: "shard threads",
    },
    {
      name: "FFmpeg",
      count: metrics?.engine?.externalFfmpegCount ?? null,
      unit: "child processes",
    },
  ];
}

/** Kernel features the data path uses, when the engine has probed them. */
export function kernelCapabilities(metrics: SystemMetrics | null): ReadonlyArray<{
  readonly label: string;
  readonly available: boolean | null;
  readonly detail: string;
}> {
  const ktls = metrics?.rtmps?.ktlsCapabilities;
  const ciphers = ktls ? Object.values(ktls).filter(Boolean).length : 0;
  return [
    {
      label: "io_uring",
      available: metrics?.ioUring?.available ?? null,
      detail: metrics?.ioUring?.deploymentTier
        ? `deployment ${metrics.ioUring.deploymentTier}`
        : "not probed",
    },
    {
      label: "Kernel TLS",
      available: ktls ? ciphers > 0 : null,
      detail: ktls ? `${ciphers} of ${Object.keys(ktls).length} cipher suites` : "not probed",
    },
  ];
}

/** The SRT ingress Owner counters that mean lost or late work when they are
 * not zero, with what each one means. */
export function ownerAttention(owner: SrtIngressOwnerHealth): readonly string[] {
  const findings: string[] = [];
  if (owner.faulted) findings.push("An Owner reported a fault.");
  if (owner.rxRingDropped > 0) {
    findings.push(`${owner.rxRingDropped} datagrams dropped: the receive ring was full.`);
  }
  if (owner.rxBufferExhaustions > 0) {
    findings.push(`${owner.rxBufferExhaustions} receives found no free buffer.`);
  }
  if (owner.eventBridgeFullVisits > 0) {
    findings.push(`${owner.eventBridgeFullVisits} visits found the event bridge to Tokio full.`);
  }
  if (owner.telemetryDropped > 0) {
    findings.push(`${owner.telemetryDropped} telemetry samples dropped.`);
  }
  if (owner.overloadDisconnects > 0) {
    findings.push(`${owner.overloadDisconnects} peers disconnected for overload.`);
  }
  if (owner.mediaSlowPasses20ms > 0) {
    findings.push(`${owner.mediaSlowPasses20ms} media passes took over 20 ms.`);
  }
  return findings;
}
