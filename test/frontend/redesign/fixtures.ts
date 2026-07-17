import { expect, type Page, type Route } from "@playwright/test";

import {
  operatorStates,
  type OperatorStateName,
} from "./fixtures/operator-states";

export interface SeededDashboardOptions {
  failOutputControl?: string;
  failRecordingControl?: string;
  failFileIngestControl?: string;
  pipelineControlDelayMs?: number;
  outputControlDelayMs?: number;
  expectOverviewReady?: boolean;
  settingsResponse?: (settings: Record<string, unknown>) => unknown;
  alertsResponse?: (alerts: Record<string, unknown>) => unknown;
  eventsResponse?: (events: Record<string, unknown>) => unknown;
  mediaResponse?: (media: Record<string, unknown>) => unknown;
  rateLimitResponse?: (rateLimits: Record<string, unknown>) => unknown;
  pipelineTelemetryResponse?: (
    pipelineId: string,
    telemetry: Record<string, unknown>,
  ) => unknown;
  logsResponse?: (logs: Record<string, unknown>[]) => unknown;
  runtimeResponse?: (
    runtime: Record<string, unknown>,
    requestCount: number,
  ) => unknown;
}

interface ControlledOutputState {
  desiredState: "started" | "stopped";
  pipelineId: string;
  outputId: string;
}

interface SeedRuntimeShape {
  health?: {
    pipelines?: Record<
      string,
      { outputs?: Record<string, Record<string, unknown>> }
    >;
  };
}

function applyControlledOutputStates(
  runtime: unknown,
  controls: ReadonlyMap<string, ControlledOutputState>,
  recordings: ReadonlyMap<string, { enabled: boolean; active: boolean }>,
): unknown {
  if (!controls.size && !recordings.size) return runtime;
  const next = structuredClone(runtime) as SeedRuntimeShape;
  for (const control of controls.values()) {
    const output =
      next.health?.pipelines?.[control.pipelineId]?.outputs?.[control.outputId];
    if (!output) continue;
    Object.assign(
      output,
      control.desiredState === "stopped"
        ? {
            status: "off",
            retrying: false,
            flapping: false,
            lastError: null,
            retryAttempts: null,
            retryRemainingMs: null,
            bitrateKbps: 0,
          }
        : {
            status: "running",
            retrying: false,
            flapping: false,
            lastError: null,
            retryAttempts: null,
            retryRemainingMs: null,
            bitrateKbps: 1_800,
            uptimeSecs: 1,
          },
    );
  }
  for (const [pipelineId, recording] of recordings) {
    const pipeline = next.health?.pipelines?.[pipelineId] as
      | { recording?: Record<string, unknown> }
      | undefined;
    if (pipeline) pipeline.recording = { ...recording };
  }
  return next;
}

function seededOverview(stateName: OperatorStateName): Record<string, unknown> {
  const fixture = operatorStates[stateName];
  const pipelines = Array.isArray(fixture.settings.pipelines)
    ? (fixture.settings.pipelines as Array<Record<string, unknown>>)
    : [];
  const outputs = Array.isArray(fixture.settings.outputs)
    ? (fixture.settings.outputs as Array<Record<string, unknown>>)
    : [];
  const runtimePipelines = (fixture.runtime.health as Record<string, unknown>)
    ?.pipelines as Record<string, { outputs?: Record<string, unknown> }> | undefined;
  const failedOutputs = outputs.filter((output) => {
    const runtimeOutput =
      runtimePipelines?.[String(output.pipelineId)]?.outputs?.[
        String(output.id)
      ] as Record<string, unknown> | undefined;
    const status = String(runtimeOutput?.status || "").toLowerCase();
    return status === "failed" || status === "error";
  }).length;
  const retryingOutputs = outputs.filter((output) => {
    const runtimeOutput =
      runtimePipelines?.[String(output.pipelineId)]?.outputs?.[
        String(output.id)
      ] as Record<string, unknown> | undefined;
    return Boolean(runtimeOutput?.retrying);
  }).length;
  return {
    generatedAt: "2026-07-14T06:30:00Z",
    totalPipelines: pipelines.length,
    activePipelines: pipelines.length,
    degradedPipelines: retryingOutputs || failedOutputs ? 1 : 0,
    failedOutputs,
    alertCount: { critical: failedOutputs, warning: retryingOutputs },
    srtListener: null,
  };
}

function seededAlerts(stateName: OperatorStateName): Record<string, unknown> {
  const alerts =
    stateName === "mixed-health"
      ? [
          {
            id: "seed-alert-retrying-output",
            severity: "warning",
            scope: "output",
            pipelineId: "pipe-retrying",
            outputId: "out-retrying",
            title: "Retrying output",
            cause: "Synthetic destination refused the connection",
            evidence: ["Retrying Output entered retry backoff"],
            recommendedAction:
              "Inspect the destination endpoint and retry budget before restarting the output.",
            generatedAt: "2026-07-14T06:29:54Z",
            firstSeen: "2026-07-14T06:29:54Z",
            lastSeen: "2026-07-14T06:29:54Z",
          },
        ]
      : [];
  return { generatedAt: "2026-07-14T06:30:00Z", alerts };
}

function seededLifecycleEvents(
  stateName: OperatorStateName,
  pipelineId: string | null,
): Record<string, unknown> {
  const events =
    stateName === "mixed-health"
      ? [
          {
            seq: 101,
            timestamp: "2026-07-14T06:29:54Z",
            kind: "egress.retrying",
            pipelineId: "pipe-retrying",
            outputId: "out-retrying",
            error: "Synthetic destination refused the connection",
          },
        ]
      : [];
  const filtered = pipelineId
    ? events.filter((event) => event.pipelineId === pipelineId)
    : events;
  return {
    generatedAt: "2026-07-14T06:30:00Z",
    count: filtered.length,
    events: filtered,
  };
}

function seededEngineTelemetry(
  stateName: OperatorStateName,
): Record<string, unknown> {
  const fixture = operatorStates[stateName];
  const outputs = Array.isArray(fixture.settings.outputs)
    ? (fixture.settings.outputs as Array<Record<string, unknown>>)
    : [];
  return {
    generatedAt: "2026-07-14T06:30:00Z",
    ingests: [
      {
        pipelineId: "pipe-healthy",
        protocol: "rtmp",
        uptimeSecs: 720,
        bytesReceived: 4_194_304,
        metrics: { packetsIn: 1200 },
      },
      {
        pipelineId: "pipe-retrying",
        protocol: "srt",
        uptimeSecs: 540,
        bytesReceived: 2_097_152,
        metrics: { packetsIn: 740 },
      },
    ],
    stages: [
      {
        pipelineId: "pipe-healthy",
        kind: "video",
        active: true,
        metrics: { packetsOut: 1180 },
      },
      {
        pipelineId: "pipe-healthy",
        kind: "audio",
        active: true,
        metrics: { packetsOut: 1180 },
      },
      {
        pipelineId: "pipe-retrying",
        kind: "mux",
        active: true,
        metrics: { packetsOut: 510 },
      },
    ],
    egresses: outputs.map((output) => ({
      pipelineId: output.pipelineId,
      outputId: output.id,
      status: output.pipelineId === "pipe-retrying" ? "retrying" : "running",
      bytesOut: output.pipelineId === "pipe-retrying" ? 65_536 : 1_048_576,
    })),
    activeTranscoderBuffers: 2,
  };
}

function seededPipelineTelemetry(pipelineId: string): Record<string, unknown> {
  const isRetrying = pipelineId === "pipe-retrying";
  return {
    generatedAt: "2026-07-14T06:30:00Z",
    pipelineId,
    ingest: {
      pipelineId,
      protocol: isRetrying ? "srt" : "rtmp",
      uptimeSecs: isRetrying ? 540 : 720,
      bytesReceived: isRetrying ? 2_097_152 : 4_194_304,
      metrics: { packetsIn: isRetrying ? 740 : 1200 },
    },
    sourceRing: {
      fill: isRetrying ? 6 : 3,
      capacity: 12,
      fillPercent: isRetrying ? 50 : 25,
      estimatedPktRatePerSec: isRetrying ? 24 : 30,
      bufferDepthSecs: isRetrying ? 1.5 : 1,
      payloadStats: {},
      readers: [
        {
          name: isRetrying ? "retrying-output-reader" : "healthy-output-reader",
          lagSlots: isRetrying ? 2 : 0,
          overflowCount: 0,
          packetAgeMs: isRetrying ? 80 : 30,
        },
      ],
    },
    stages: [
      {
        stageKey: `${pipelineId}:video`,
        pipelineId,
        kind: "video",
        active: true,
        metrics: { packetsOut: isRetrying ? 500 : 1180 },
      },
      {
        stageKey: `${pipelineId}:audio`,
        pipelineId,
        kind: "audio",
        active: true,
        metrics: { packetsOut: isRetrying ? 500 : 1180 },
      },
    ],
    egresses: [
      {
        pipelineId,
        outputId: isRetrying ? "out-retrying" : "out-healthy",
        status: isRetrying ? "retrying" : "running",
        bytesOut: isRetrying ? 65_536 : 1_048_576,
        lastError: isRetrying
          ? "Synthetic destination refused the connection"
          : null,
      },
    ],
  };
}

function seededStageTelemetry(stageKey: string): Record<string, unknown> {
  const [pipelineId = "pipe-healthy", kind = "video"] = stageKey.split(":");
  const isRetrying = pipelineId === "pipe-retrying";
  return {
    generatedAt: "2026-07-14T06:30:00Z",
    stageKey,
    pipelineId,
    kind,
    active: true,
    metrics: {
      packetsOut: isRetrying ? 500 : 1180,
      queueDepth: isRetrying ? 2 : 0,
    },
    pipeMetrics: {
      packetsIn: isRetrying ? 500 : 1180,
    },
  };
}

async function fulfillJson(route: Route, body: unknown): Promise<void> {
  await route.fulfill({
    status: 200,
    contentType: "application/json",
    body: JSON.stringify(body),
  });
}

async function login(page: Page): Promise<void> {
  await page.goto("/login");
  await page.locator("#password-input").fill("admin");
  await page.locator("#login-btn").click();
  await page.waitForURL(/\/$/);
}

export async function openSeededDashboard(
  page: Page,
  stateName: OperatorStateName,
  href = "/?mode=overview",
  options: SeededDashboardOptions = {},
): Promise<void> {
  const requested = new URL(href, "http://seed.local");
  const fixture = operatorStates[stateName];
  const settings =
    options.settingsResponse?.(structuredClone(fixture.settings)) ??
    fixture.settings;
  let runtimeRequestCount = 0;
  const controlledOutputs = new Map<string, ControlledOutputState>();
  const controlledRecordings = new Map<
    string,
    { enabled: boolean; active: boolean }
  >();
  await login(page);

  await page.addInitScript((uiVersion: string | null) => {
    window.localStorage.setItem(
      "restream.dashboardUiVersion.v1",
      uiVersion === "v2" ? "v2" : "v1",
    );
    Object.defineProperty(window, "EventSource", {
      configurable: true,
      value: undefined,
    });
  }, requested.searchParams.get("ui"));

  await page.route("**/api/v1/**", async (route) => {
    const url = new URL(route.request().url());
    const recordingControlMatch = url.pathname.match(
      /^\/api\/v1\/pipelines\/([^/]+)\/recording\/(start|stop)$/,
    );
    if (route.request().method() === "POST" && recordingControlMatch) {
      if (options.pipelineControlDelayMs) {
        await new Promise((resolve) =>
          setTimeout(resolve, options.pipelineControlDelayMs),
        );
      }
      if (options.failRecordingControl) {
        await route.fulfill({
          status: 500,
          contentType: "application/json",
          body: JSON.stringify({ error: options.failRecordingControl }),
        });
        return;
      }
      const [, encodedPipelineId, action] = recordingControlMatch;
      const pipelineId = decodeURIComponent(encodedPipelineId);
      const recording =
        action === "start"
          ? { enabled: true, active: true }
          : { enabled: false, active: false };
      controlledRecordings.set(pipelineId, recording);
      await fulfillJson(route, recording);
      return;
    }
    const fileIngestControlMatch = url.pathname.match(
      /^\/api\/v1\/ingests\/([^/]+)\/(start|stop)$/,
    );
    if (route.request().method() === "POST" && fileIngestControlMatch) {
      if (options.pipelineControlDelayMs) {
        await new Promise((resolve) =>
          setTimeout(resolve, options.pipelineControlDelayMs),
        );
      }
      if (options.failFileIngestControl) {
        await route.fulfill({
          status: 500,
          contentType: "application/json",
          body: JSON.stringify({ error: options.failFileIngestControl }),
        });
        return;
      }
      const [, encodedIngestId, action] = fileIngestControlMatch;
      const ingestId = decodeURIComponent(encodedIngestId);
      const pipelines = Array.isArray(
        (settings as Record<string, unknown>).pipelines,
      )
        ? ((settings as Record<string, unknown>).pipelines as Array<
            Record<string, unknown>
          >)
        : [];
      const fileIngest = pipelines
        .map((pipeline) => pipeline.fileIngest as Record<string, unknown>)
        .find((candidate) => candidate?.id === ingestId);
      if (!fileIngest) {
        throw new Error(`Unknown seeded file-ingest target: ${url.pathname}`);
      }
      await fulfillJson(route, {
        ...fileIngest,
        running: action === "start",
      });
      return;
    }
    const outputControlMatch = url.pathname.match(
      /^\/api\/v1\/pipelines\/([^/]+)\/outputs\/([^/]+)\/(start|stop)$/,
    );
    if (route.request().method() === "POST" && outputControlMatch) {
      const [, encodedPipelineId, encodedOutputId, action] =
        outputControlMatch;
      const pipelineId = decodeURIComponent(encodedPipelineId);
      const outputId = decodeURIComponent(encodedOutputId);
      const outputs = Array.isArray(
        (settings as Record<string, unknown>).outputs,
      )
        ? ((settings as Record<string, unknown>).outputs as Array<
            Record<string, unknown>
          >)
        : [];
      const output = outputs.find(
        (candidate) =>
          candidate.pipelineId === pipelineId && candidate.id === outputId,
      );
      if (!output) {
        throw new Error(`Unknown seeded output control target: ${url.pathname}`);
      }
      if (options.outputControlDelayMs) {
        await new Promise((resolve) =>
          setTimeout(resolve, options.outputControlDelayMs),
        );
      }
      if (options.failOutputControl) {
        await route.fulfill({
          status: 500,
          contentType: "application/json",
          body: JSON.stringify({ error: options.failOutputControl }),
        });
        return;
      }
      controlledOutputs.set(`${pipelineId}:${outputId}`, {
        desiredState: action === "start" ? "started" : "stopped",
        pipelineId,
        outputId,
      });
      await fulfillJson(route, {
        message: `Output ${action === "start" ? "started" : "stopped"}`,
        desiredState: action === "start" ? "started" : "stopped",
        output: {
          ...output,
          desiredState: action === "start" ? "started" : "stopped",
        },
      });
      return;
    }
    const pipelineSummaryMatch = url.pathname.match(
      /^\/api\/v1\/pipelines\/([^/]+)\/summary$/,
    );
    if (pipelineSummaryMatch) {
      const [, encodedPipelineId] = pipelineSummaryMatch;
      const pipelineId = decodeURIComponent(encodedPipelineId);
      await fulfillJson(route, {
        pipelineId,
        input: { status: "on" },
        outputs: { total: 1, running: 1 },
        graph: { hasGraph: true, nodes: 3, activeNodes: 3 },
        alerts: [],
      });
      return;
    }
    const pipelineGraphMatch = url.pathname.match(
      /^\/api\/v1\/pipelines\/([^/]+)\/graph$/,
    );
    if (pipelineGraphMatch) {
      const [, encodedPipelineId] = pipelineGraphMatch;
      const pipelineId = decodeURIComponent(encodedPipelineId);
      await fulfillJson(route, {
        pipelineId,
        nodes: [],
        edges: [],
      });
      return;
    }
    const pipelineTelemetryMatch = url.pathname.match(
      /^\/api\/v1\/pipelines\/([^/]+)\/telemetry$/,
    );
    if (pipelineTelemetryMatch) {
      const [, encodedPipelineId] = pipelineTelemetryMatch;
      const pipelineId = decodeURIComponent(encodedPipelineId);
      const telemetry = seededPipelineTelemetry(pipelineId);
      await fulfillJson(
        route,
        options.pipelineTelemetryResponse?.(pipelineId, telemetry) ??
          telemetry,
      );
      return;
    }
    const stageTelemetryMatch = url.pathname.match(
      /^\/api\/v1\/stages\/([^/]+)\/telemetry$/,
    );
    if (stageTelemetryMatch) {
      const [, encodedStageKey] = stageTelemetryMatch;
      await fulfillJson(
        route,
        seededStageTelemetry(decodeURIComponent(encodedStageKey)),
      );
      return;
    }
    switch (url.pathname) {
      case "/api/v1/overview":
        await fulfillJson(route, seededOverview(stateName));
        return;
      case "/api/v1/alerts":
        {
          const alerts = seededAlerts(stateName);
          await fulfillJson(route, options.alertsResponse?.(alerts) ?? alerts);
        }
        return;
      case "/api/v1/events":
        {
          const events = seededLifecycleEvents(
            stateName,
            url.searchParams.get("pipeline_id"),
          );
          await fulfillJson(route, options.eventsResponse?.(events) ?? events);
        }
        return;
      case "/api/v1/logs/stream":
        await route.fulfill({
          status: 200,
          contentType: "text/event-stream",
          body: "retry: 60000\n\n",
        });
        return;
      case "/api/v1/settings":
        await fulfillJson(route, settings);
        return;
      case "/api/v1/dashboard/runtime":
        runtimeRequestCount += 1;
        await fulfillJson(
          route,
          applyControlledOutputStates(
            options.runtimeResponse?.(fixture.runtime, runtimeRequestCount) ??
              fixture.runtime,
            controlledOutputs,
            controlledRecordings,
          ),
        );
        return;
      case "/api/v1/audio-caps":
        await fulfillJson(route, { caps: {}, platformLabels: {} });
        return;
      case "/api/v1/logs":
        await fulfillJson(
          route,
          options.logsResponse?.(fixture.logs) ?? { logs: fixture.logs },
        );
        return;
      case "/api/v1/engine":
        await fulfillJson(route, {
          restream: {
            version: "seeded",
            commit: "seeded",
            nativeBuildId: "seeded",
          },
        });
        return;
      case "/api/v1/engine/health":
        await fulfillJson(route, {
          status: "ready",
          hostSettings: [
            {
              key: "runtime.nofile",
              label: "Open file descriptors",
              current: 65536,
              required: 65536,
              unit: "fds",
              status: "ok",
            },
          ],
        });
        return;
      case "/api/v1/engine/telemetry":
        await fulfillJson(route, seededEngineTelemetry(stateName));
        return;
      case "/api/v1/security/rate-limits":
        {
          const rateLimits = {
            attempts:
              stateName === "mixed-health"
                ? [
                    {
                      scope: "dashboard-login",
                      ip: "203.0.113.10",
                      failureCount: 2,
                      banned: false,
                    },
                  ]
                : [],
          };
          await fulfillJson(
            route,
            options.rateLimitResponse?.(rateLimits) ?? rateLimits,
          );
        }
        return;
      case "/api/v1/engine/resource-map":
        await fulfillJson(route, { resources: [] });
        return;
      case "/api/v1/stream-keys":
        await fulfillJson(route, []);
        return;
      case "/api/v1/media":
        {
          const media = {
            files: [
              {
                name: "synthetic-source.mp4",
                kind: "upload",
                size: 1_048_576,
                modifiedAt: "2026-07-14T00:00:00Z",
              },
            ],
          };
          await fulfillJson(route, options.mediaResponse?.(media) ?? media);
        }
        return;
      case "/api/v1/media/synthetic-source.mp4/analysis":
        await fulfillJson(route, {
          videoCodec: "h264",
          fps: 30,
          durationSec: 60,
          averageKeyframeIntervalSec: 2,
          maxKeyframeIntervalSec: 4,
        });
        return;
      default:
        throw new Error(`Unmodeled redesign seed request: ${url.pathname}`);
    }
  });

  await page.goto(href);
  if (options.expectOverviewReady === false) {
    const mode = requested.searchParams.get("mode") ?? "overview";
    const workspaceMode =
      mode === "inspect" || mode === "control" ? "pipeline" : mode;
    if (workspaceMode === "incidents" || workspaceMode === "telemetry") {
      return;
    }
    await expect(
      page.locator(`[data-dashboard-mode="${workspaceMode}"]`),
    ).toHaveAttribute("aria-selected", "true");
    if (workspaceMode === "pipeline") {
      const view =
        mode === "inspect"
          ? "inspect"
          : mode === "control"
            ? "monitor"
            : (requested.searchParams.get("view") ?? "operate");
      await expect(
        page.locator(`[data-pipeline-workspace-view="${view}"]`),
      ).toHaveAttribute("aria-selected", "true");
    }
    return;
  }
  const overview = href.includes("ui=v2")
    ? page.locator("#dashboard-v2-overview")
    : page.locator("#overview-mode-content");
  await expect(
    overview.getByRole("heading", { name: "Fleet overview" }),
  ).toBeVisible({ timeout: 15_000 });
}
