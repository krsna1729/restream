import { expect, type Page, type Route } from "@playwright/test";

import {
  operatorStates,
  type OperatorStateName,
} from "./fixtures/operator-states";

export interface SeededDashboardOptions {
  pipelineControlDelayMs?: number;
  outputControlDelayMs?: number;
  expectOverviewReady?: boolean;
  settingsResponse?: (settings: Record<string, unknown>) => unknown;
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

  await page.addInitScript(() => {
    Object.defineProperty(window, "EventSource", {
      configurable: true,
      value: undefined,
    });
  });

  await page.route("**/api/v1/**", async (route) => {
    const url = new URL(route.request().url());
    const recordingControlMatch = url.pathname.match(
      /^\/api\/v1\/pipelines\/([^/]+)\/recording\/(start|stop)$/,
    );
    if (route.request().method() === "POST" && recordingControlMatch) {
      const [, encodedPipelineId, action] = recordingControlMatch;
      const pipelineId = decodeURIComponent(encodedPipelineId);
      if (options.pipelineControlDelayMs) {
        await new Promise((resolve) =>
          setTimeout(resolve, options.pipelineControlDelayMs),
        );
      }
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
      if (options.pipelineControlDelayMs) {
        await new Promise((resolve) =>
          setTimeout(resolve, options.pipelineControlDelayMs),
        );
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
    switch (url.pathname) {
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
        await fulfillJson(route, { logs: fixture.logs });
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
      case "/api/v1/security/rate-limits":
        await fulfillJson(route, { attempts: [] });
        return;
      case "/api/v1/engine/resource-map":
        await fulfillJson(route, { resources: [] });
        return;
      case "/api/v1/stream-keys":
        await fulfillJson(route, []);
        return;
      case "/api/v1/media":
        await fulfillJson(route, {
          files: [
            {
              name: "synthetic-source.mp4",
              kind: "upload",
              size: 1_048_576,
              modifiedAt: "2026-07-14T00:00:00Z",
            },
          ],
        });
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
    const requested = new URL(href, "http://seed.local");
    const mode = requested.searchParams.get("mode") ?? "overview";
    const workspaceMode =
      mode === "inspect" || mode === "control" ? "pipeline" : mode;
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
  ).toBeVisible();
}
