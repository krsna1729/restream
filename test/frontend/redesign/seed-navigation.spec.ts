import { expect, test } from "@playwright/test";

import { openSeededDashboard } from "./fixtures";
import {
  expectPushStateCount,
  expectTabVisibleInRail,
  getCdpLayoutWidthDelta,
  getCdpNamesByRole,
  getCdpNodeCount,
  getCdpStatusTexts,
  getDocumentWidthOverflow,
  installPushStateCounter,
  resetPushStateCounter,
  tabUntilFocused,
} from "./seed-helpers";

test("seed: default dashboard overview Operate is one predictable history step @desktop", async ({
  page,
}) => {
  await installPushStateCounter(page);
  await openSeededDashboard(page, "mixed-health", "/?mode=overview");
  await resetPushStateCounter(page);

  await page
    .locator("#dashboard-v2-overview")
    .locator("article")
    .filter({ hasText: "Retrying Destination" })
    .getByRole("button", { name: "Operate Retrying Destination", exact: true })
    .click();
  await expect(page).toHaveURL(/mode=pipeline/);
  await expect(page).toHaveURL(/view=operate/);
  await expect(page).toHaveURL(/p=pipe-retrying/);
  await expect(
    page.locator("#dashboard-v2-pipeline-header-root").getByRole("heading", {
      name: "Retrying Destination",
    }),
  ).toBeVisible();
  await expectPushStateCount(page, 1);

  await page.goBack();
  await expect(page).toHaveURL(/\?mode=overview$/);
  await expect(
    page
      .locator("#dashboard-v2-overview")
      .getByRole("heading", { name: "Fleet overview" }),
  ).toBeVisible();
  expect(await getCdpNodeCount(page)).toBeLessThan(6_000);
});

test("seed: default dashboard overview Inspect is one predictable history step @desktop", async ({
  page,
}) => {
  await installPushStateCounter(page);
  await openSeededDashboard(page, "mixed-health", "/?mode=overview", {
    resourceMapResponse: (pipelineId, resourceMap) => ({
      ...resourceMap,
      scope: { kind: "pipeline", pipelineId },
      view: "detail",
      limits: {
        topN: 50,
        totalNodeCount: 3,
        returnedNodeCount: 3,
        truncatedNodeCount: 0,
      },
      summary: {
        cpuPercent: 11.25,
        totalMemoryBytes: 96 * 1024 * 1024,
        processThreadCount: 18,
        srtSenderThreads: 2,
        srtSenderThreadLimit: 512,
        externalFfmpegCount: 1,
        retainedPayloadBytes: 4096,
      },
      nodes: [
        {
          id: `${pipelineId}:video:720p`,
          kind: "stage",
          label: "video:720p",
          pipelineId,
          execution: "child_process",
          cpuPercent: 12.5,
          memory: {
            attributedBytes: 64 * 1024 * 1024,
            confidence: "measured",
          },
        },
      ],
    }),
  });
  await resetPushStateCounter(page);

  await page
    .locator("#dashboard-v2-overview")
    .locator("article")
    .filter({ hasText: "Retrying Destination" })
    .getByRole("button", { name: "Inspect Retrying Destination", exact: true })
    .click();
  await expect(page).toHaveURL(/mode=pipeline/);
  await expect(page).toHaveURL(/view=inspect/);
  await expect(page).toHaveURL(/p=pipe-retrying/);
  await expect(page.locator("#inspect-mode-panel")).toBeVisible();
  await expect(
    page.locator("#dashboard-v2-pipeline-inspect-root"),
  ).toBeVisible();
  await expect(page.locator("#inspect-pipeline-select")).toHaveValue(
    "pipe-retrying",
  );
  await expect(page.locator("#dashboard-v2-pipeline-inspect-root")).toContainText(
    "Inspecting Retrying Destination · input live · 1 output · 1 attention item",
  );
  const inspectCheckpoint = page.locator("#dashboard-v2-pipeline-inspect-root");
  await expect(
    inspectCheckpoint.locator("#dashboard-v2-pipeline-inspect-title"),
  ).toBeVisible();
  await expect(inspectCheckpoint.getByText("Output retrying").first()).toBeVisible();
  await expect(
    inspectCheckpoint.getByText("1 fault candidate", { exact: true }),
  ).toBeVisible();
  await expect(
    inspectCheckpoint.getByRole("button", {
      name: "Operate inspected pipeline",
    }),
  ).toBeEnabled();
  const inspectButtonNames = await getCdpNamesByRole(page, "button");
  expect(inspectButtonNames).toContain("Operate inspected pipeline");
  expect(inspectButtonNames).toContain("Run diagnostics for inspected pipeline");
  expect(inspectButtonNames).not.toContain("Open");
  expect(inspectButtonNames).not.toContain("Run Diagnostics");
  await expect(
    inspectCheckpoint.getByRole("button", {
      name: "Run diagnostics for inspected pipeline",
    }),
  ).toBeEnabled();
  const inspectCheckpointButtonNames = await getCdpNamesByRole(page, "button");
  expect(inspectCheckpointButtonNames).toEqual(
    expect.arrayContaining([
      "Operate inspected pipeline",
      "Run diagnostics for inspected pipeline",
    ]),
  );
  expect(inspectCheckpointButtonNames).not.toEqual(
    expect.arrayContaining(["Operate", "Diagnostics"]),
  );
  await expect(page.locator("#inspect-focus-summary")).toHaveText(
    "Inspection focus · 1 blocker before active probes · 1 fault candidate · Inspect recent errors and retry backoff before forcing a restart.",
  );
  expect(await getCdpStatusTexts(page)).toContain(
    "Inspecting Retrying Destination · input live · 1 output · 1 attention item",
  );
  expect(await getCdpStatusTexts(page)).toContain(
    "Inspection focus · 1 blocker before active probes · 1 fault candidate · Inspect recent errors and retry backoff before forcing a restart.",
  );
  const probeDetails = page.locator("#inspect-diagnostics-summary");
  await expect(
    probeDetails.getByRole("button", {
      name: "Show probe details for Retrying Destination",
    }),
  ).toHaveAttribute("aria-expanded", "false");
  await expect(probeDetails.getByText("Probe Readiness")).toHaveCount(0);
  await probeDetails
    .getByRole("button", {
      name: "Show probe details for Retrying Destination",
    })
    .click();
  await expect(
    probeDetails.getByRole("button", {
      name: "Hide probe details for Retrying Destination",
    }),
  ).toHaveAttribute("aria-expanded", "true");
  await expect(probeDetails.getByText("Probe Readiness")).toBeVisible();
  await expect(probeDetails.getByText("Fault Candidates")).toBeVisible();
  await probeDetails
    .getByRole("button", {
      name: "Hide probe details for Retrying Destination",
    })
    .click();
  await expect(probeDetails.getByText("Probe Readiness")).toHaveCount(0);
  const resourceDetails = page.locator("#inspect-resource-details");
  await expect(resourceDetails.getByText("Process Metrics")).toBeVisible();
  await expect(resourceDetails.getByText("Pipeline Attribution")).toBeVisible();
  await expect(
    resourceDetails.getByRole("button", {
      name: "Show resource details for Retrying Destination",
    }),
  ).toHaveAttribute("aria-expanded", "false");
  await expect(resourceDetails.getByText("FFmpeg workers")).toHaveCount(0);
  const collapsedInspectButtonNames = await getCdpNamesByRole(page, "button");
  expect(collapsedInspectButtonNames).toEqual(
    expect.arrayContaining([
      "Stop graph auto refresh",
      "Show resource details for Retrying Destination",
    ]),
  );
  expect(collapsedInspectButtonNames).not.toContain("Stop Refresh");
  expect(collapsedInspectButtonNames).not.toContain("Show resource details");
  await resourceDetails
    .getByRole("button", {
      name: "Show resource details for Retrying Destination",
    })
    .click();
  await expect(
    resourceDetails.getByRole("button", {
      name: "Hide resource details for Retrying Destination",
    }),
  ).toHaveAttribute("aria-expanded", "true");
  await expect(resourceDetails.getByText("FFmpeg workers")).toBeVisible();
  await expect(resourceDetails.getByText("video:720p")).toBeVisible();
  await resourceDetails
    .getByRole("button", {
      name: "Hide resource details for Retrying Destination",
    })
    .click();
  await expect(resourceDetails.getByText("FFmpeg workers")).toHaveCount(0);
  await expectPushStateCount(page, 1);

  await page.goBack();
  await expect(page).toHaveURL(/\?mode=overview$/);
  await expect(
    page
      .locator("#dashboard-v2-overview")
      .getByRole("heading", { name: "Fleet overview" }),
  ).toBeVisible();
  expect(await getCdpNodeCount(page)).toBeLessThan(7_000);
});

test("seed: default dashboard Inspect output search narrows noisy sibling outputs @desktop", async ({
  page,
}) => {
  await openSeededDashboard(
    page,
    "chaos-recovery",
    "/?mode=pipeline&view=inspect&p=pipe-stall",
    { expectOverviewReady: false },
  );

  const inspect = page.locator("#inspect-mode-panel");
  await expect(page.locator("#dashboard-v2-pipeline-inspect-root")).toContainText(
    "Inspecting Stalled Sink Isolation · input live · 6 outputs · 1 attention item",
  );
  await expect(inspect.getByLabel("Search inspect outputs")).toBeVisible();
  const outputPreview = inspect.getByLabel("Inspect output preview");
  await expect(outputPreview.getByText("RTMP stalled sink")).toBeVisible();
  await expect(outputPreview.getByText("Healthy sibling 05")).toBeVisible();

  await inspect.getByLabel("Search inspect outputs").fill("sibling 05");
  await expect(outputPreview.getByText("Healthy sibling 05")).toBeVisible();
  await expect(outputPreview.getByText("RTMP stalled sink")).not.toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    '1/6 inspect outputs match · "sibling 05"',
  );

  const clearOutputSearch = inspect.getByRole("button", {
    name: "Clear output search",
  });
  await expect(clearOutputSearch).toBeVisible();
  await clearOutputSearch.click();
  await expect(inspect.getByLabel("Search inspect outputs")).toHaveValue("");
  await expect(outputPreview.getByText("RTMP stalled sink")).toBeVisible();

  await inspect.getByLabel("Search inspect outputs").fill("not-there");
  await expect(
    outputPreview.getByText(
      'No inspect outputs match "not-there". Clear output search to show all.',
    ),
  ).toBeVisible();
  expect(await getCdpStatusTexts(page)).toEqual(
    expect.arrayContaining([
      '0/6 inspect outputs match · "not-there"',
      'No inspect outputs match "not-there". Clear output search to show all.',
    ]),
  );
  expect(await getCdpNodeCount(page)).toBeLessThan(7_500);
});

test("seed: default dashboard Inspect output search understands down aliases @desktop", async ({
  page,
}) => {
  await openSeededDashboard(
    page,
    "chaos-recovery",
    "/?mode=pipeline&view=inspect&p=pipe-stall",
    {
      expectOverviewReady: false,
      runtimeResponse: (runtime) => {
        const next = structuredClone(runtime);
        const pipeline = (
          next.health as Record<string, Record<string, unknown>>
        ).pipelines?.["pipe-stall"] as
          { outputs?: Record<string, Record<string, unknown>> } | undefined;
        if (pipeline?.outputs?.["out-stall-healthy-05"]) {
          Object.assign(pipeline.outputs["out-stall-healthy-05"], {
            status: "failed",
            rawStatus: "running",
            phase: "failed",
            failurePhase: "send",
            lastError: "synthetic sink down",
            retrying: false,
            flapping: false,
          });
        }
        return next;
      },
    },
  );

  const inspect = page.locator("#inspect-mode-panel");
  const outputPreview = inspect.getByLabel("Inspect output preview");
  await expect(page.locator("#dashboard-v2-pipeline-inspect-root")).toContainText(
    "Inspecting Stalled Sink Isolation · input live · 6 outputs · 2 attention items",
  );
  await expect(outputPreview.getByText("RTMP stalled sink")).toBeVisible();
  await expect(outputPreview.getByText("Healthy sibling 05")).toBeVisible();

  await inspect.getByLabel("Search inspect outputs").fill("down");
  await expect(outputPreview.getByText("Healthy sibling 05")).toBeVisible();
  await expect(outputPreview.getByText("RTMP stalled sink")).not.toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    '1/6 inspect outputs match · "down"',
  );

  await inspect.getByLabel("Search inspect outputs").fill("running");
  await expect(outputPreview.getByText("Healthy sibling 01")).toBeVisible();
  await expect(outputPreview.getByText("Healthy sibling 05")).not.toBeVisible();
  await expect(outputPreview.getByText("RTMP stalled sink")).not.toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    '4/6 inspect outputs match · "running"',
  );

  await inspect.getByLabel("Search inspect outputs").fill("offline");
  await expect(
    outputPreview.getByText(
      'No inspect outputs match "offline". Clear output search to show all.',
    ),
  ).toBeVisible();
  expect(await getCdpStatusTexts(page)).toEqual(
    expect.arrayContaining([
      '0/6 inspect outputs match · "offline"',
      'No inspect outputs match "offline". Clear output search to show all.',
    ]),
  );
  expect(await getCdpNodeCount(page)).toBeLessThan(7_500);
});

test("seed: default dashboard pipeline workspace tabs preserve one selected context @desktop", async ({
  page,
}) => {
  await installPushStateCounter(page);
  await openSeededDashboard(
    page,
    "mixed-health",
    "/?mode=pipeline&view=operate&p=pipe-retrying",
    { expectOverviewReady: false },
  );
  await resetPushStateCounter(page);

  const operateTab = page.locator("#pipeline-workspace-tab-operate");
  const inspectTab = page.locator("#pipeline-workspace-tab-inspect");
  const monitorTab = page.locator("#pipeline-workspace-tab-monitor");

  await expect(operateTab).toHaveAttribute("aria-selected", "true");
  await operateTab.click();
  await expectPushStateCount(page, 0);
  await expect(page).toHaveURL(/view=operate/);
  await expect(page).toHaveURL(/p=pipe-retrying/);

  await inspectTab.click();
  await expectPushStateCount(page, 1);
  await expect(page).toHaveURL(/view=inspect/);
  await expect(page).toHaveURL(/p=pipe-retrying/);
  await expect(inspectTab).toHaveAttribute("aria-selected", "true");
  await expect(page.locator("#inspect-mode-panel")).toBeVisible();
  await expect(page.locator("#inspect-pipeline-select")).toHaveValue(
    "pipe-retrying",
  );

  await monitorTab.click();
  await expectPushStateCount(page, 2);
  await expect(page).toHaveURL(/view=monitor/);
  await expect(page).toHaveURL(/p=pipe-retrying/);
  await expect(monitorTab).toHaveAttribute("aria-selected", "true");
  await expect(page.locator("#control-mode-panel")).toBeVisible();
  await expect(page.locator("#control-room-pipeline-select")).toHaveValue(
    "pipe-retrying",
  );
  expect(await getCdpNodeCount(page)).toBeLessThan(7_500);

  await monitorTab.click();
  await expectPushStateCount(page, 2);

  await page.goBack();
  await expect(page).toHaveURL(/view=inspect/);
  await expect(page).toHaveURL(/p=pipe-retrying/);
  await expect(inspectTab).toHaveAttribute("aria-selected", "true");
  await page.goBack();
  await expect(page).toHaveURL(/view=operate/);
  await expect(page).toHaveURL(/p=pipe-retrying/);
  await expect(operateTab).toHaveAttribute("aria-selected", "true");
});

test("seed: default dashboard top-level Pipeline tab restores last workspace context @desktop", async ({
  page,
}) => {
  await openSeededDashboard(
    page,
    "mixed-health",
    "/?mode=pipeline&view=monitor&p=pipe-retrying",
    { expectOverviewReady: false },
  );

  await expect(page.locator("#pipeline-workspace-tab-monitor")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(page.locator("#control-room-pipeline-select")).toHaveValue(
    "pipe-retrying",
  );
  await expect(page.locator("#dashboard-v2-control-room-root")).toContainText(
    "Monitoring Retrying Destination · 1 output · 1 monitor · 0 missing URLs",
  );

  await page.locator("#workspace-tab-incidents").click();
  await expect(page).toHaveURL(/mode=incidents/);
  await expect(page.locator("#workspace-tab-incidents")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(page.locator("#dashboard-v2-incidents-root")).toContainText(
    "0 critical · 1 warning · 1 recent event · fleet",
  );

  await page.locator("#workspace-tab-pipeline").click();
  await expect(page).toHaveURL(/mode=pipeline/);
  await expect(page).toHaveURL(/view=monitor/);
  await expect(page).toHaveURL(/p=pipe-retrying/);
  await expect(page.locator("#workspace-tab-pipeline")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(page.locator("#pipeline-workspace-tab-monitor")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(page.locator("#control-room-pipeline-select")).toHaveValue(
    "pipe-retrying",
  );
  await expect(page.locator("#workspace-mode-summary")).toHaveText(
    "Dashboard · Pipeline monitoring wall",
  );
  expect(await getCdpStatusTexts(page)).toContain(
    "Dashboard · Pipeline monitoring wall",
  );

  await page.goBack();
  await expect(page).toHaveURL(/mode=incidents/);
  await expect(page.locator("#workspace-tab-incidents")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await page.goBack();
  await expect(page).toHaveURL(/mode=pipeline/);
  await expect(page).toHaveURL(/view=monitor/);
  await expect(page).toHaveURL(/p=pipe-retrying/);
  await expect(page.locator("#pipeline-workspace-tab-monitor")).toHaveAttribute(
    "aria-selected",
    "true",
  );
});

test("seed: default dashboard Monitor search does not mislabel filtered outputs as missing @desktop", async ({
  page,
}) => {
  await openSeededDashboard(
    page,
    "mixed-health",
    "/?mode=pipeline&view=monitor&p=pipe-retrying",
    {
      expectOverviewReady: false,
      pipelineInputsResponse: (pipelineId) => ({
        selectedInputId: "input-primary",
        inputs: [
          {
            id: "input-primary",
            pipelineId,
            label: "Primary",
            streamKey: "synthetic-retrying-key",
            role: "primary",
            enabled: true,
            selected: true,
            ingestUrls: {
              rtmp:
                "rtmp://ingest.example.invalid/live/synthetic-retrying-key",
              srt: null,
            },
            previewUrl: "/hls/inputs/input-primary/master.m3u8",
            runtime: {
              connected: true,
              forwardingState: "active",
              protocol: "rtmp",
              uptimeSeconds: 540,
              bytesReceived: 31_000_000,
              remoteAddr: null,
              video: null,
              audio: null,
              quality: null,
            },
          },
        ],
      }),
    },
  );

  const monitor = page.locator("#control-mode-panel");
  const checkpoint = page.locator("#dashboard-v2-control-room-root");
  const search = monitor.locator("#control-room-search-input");
  const summary = monitor.locator("#control-room-summary");
  await expect(checkpoint.locator("#dashboard-v2-control-room-title")).toBeVisible();
  await expect(
    monitor.getByRole("heading", { name: "Monitor controls" }),
  ).toBeVisible();
  await expect(
    monitor.getByRole("heading", { name: "Monitor previews" }),
  ).toBeVisible();
  await expect(
    monitor.getByRole("combobox", { name: "Filter monitor by pipeline" }),
  ).toBeVisible();
  await expect(monitor.getByLabel("Search monitor outputs")).toBeVisible();
  await expect(checkpoint).toContainText(
    "Monitoring Retrying Destination · 1 output · 1 monitor · 0 missing URLs",
  );
  expect(await getCdpStatusTexts(page)).toContain(
    "Monitoring Retrying Destination · 1 output · 1 monitor · 0 missing URLs",
  );
  await expect(
    checkpoint.locator("#dashboard-v2-control-room-title"),
  ).toBeVisible();
  await expect(
    checkpoint.getByText("1/1 monitored", { exact: true }),
  ).toBeVisible();
  await expect(checkpoint.getByText("No active search")).toBeVisible();
  await expect(summary).toHaveText("1/1 monitored · 0 missing monitoring URLs");
  const initialMonitorButtonNames = await getCdpNamesByRole(page, "button");
  expect(initialMonitorButtonNames).toEqual(
    expect.arrayContaining([
      "Pause all monitor previews",
      "Unmute all monitor previews",
      "Reset monitor wall",
      "Previous monitor page",
      "Next monitor page",
    ]),
  );
  expect(initialMonitorButtonNames).not.toContain("Play All");
  expect(initialMonitorButtonNames).not.toContain("Pause All");
  expect(initialMonitorButtonNames).not.toContain("Reset");
  expect(await getCdpNamesByRole(page, "combobox")).toContain(
    "Filter monitor by pipeline",
  );
  expect(await getCdpNamesByRole(page, "combobox")).not.toContain(
    "Monitor pipeline",
  );
  expect(await getCdpNamesByRole(page, "combobox")).not.toContain(
    "Healthy ProgramRetrying Destination",
  );
  expect(await getCdpNamesByRole(page, "textbox")).toContain(
    "Search monitor outputs",
  );
  expect(await getCdpNamesByRole(page, "textbox")).not.toContain(
    "Search outputs...",
  );
  const initialMonitorHeadingNames = await getCdpNamesByRole(page, "heading");
  expect(initialMonitorHeadingNames).toEqual(
    expect.arrayContaining([
      "Control Room",
      "Monitor controls",
      "Monitor previews",
      "Primary",
      "Retrying Output",
    ]),
  );

  await search.fill("retrying");
  await expect(summary).toHaveText(
    '1/1 monitored match · 0 missing monitoring URLs · "retrying"',
  );
  await expect(monitor.getByText("Retrying Output")).toBeVisible();
  await expect(
    checkpoint.getByText('1/1 match "retrying"', { exact: true }),
  ).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    '1/1 monitored match · 0 missing monitoring URLs · "retrying"',
  );

  await search.fill("nowhere");
  await expect(summary).toHaveText(
    '0/1 monitored match · 0 missing monitoring URLs · "nowhere"',
  );
  await expect(
    monitor.getByText(
      'No monitoring outputs match "nowhere". Clear search to show all monitoring cards.',
    ),
  ).toBeVisible();
  await expect(
    checkpoint.getByText('0/1 match "nowhere"', { exact: true }),
  ).toBeVisible();
  const clearSearch = monitor.getByRole("button", {
    name: "Clear monitor search",
  });
  await expect(clearSearch).toBeVisible();
  const filteredMonitorButtonNames = await getCdpNamesByRole(page, "button");
  expect(filteredMonitorButtonNames).toContain("Clear monitor search");
  expect(filteredMonitorButtonNames).not.toContain("Clear search");
  expect(await getCdpStatusTexts(page)).toEqual(
    expect.arrayContaining([
      '0/1 monitored match · 0 missing monitoring URLs · "nowhere"',
    ]),
  );

  await clearSearch.click();
  await expect(search).toHaveValue("");
  await expect(summary).toHaveText("1/1 monitored · 0 missing monitoring URLs");
  await expect(checkpoint.getByText("No active search")).toBeVisible();
  await expect(clearSearch).toBeHidden();
  await expect(monitor.getByText("Retrying Output")).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    "1/1 monitored · 0 missing monitoring URLs",
  );
  expect(await getCdpNodeCount(page)).toBeLessThan(7_500);
});

test("seed: monitor wall renders and promotes connected pipeline inputs", async ({
  page,
}, testInfo) => {
  await openSeededDashboard(
    page,
    "mixed-health",
    "/?mode=pipeline&view=monitor&p=pipe-retrying",
    {
      expectOverviewReady: false,
      pipelineInputsResponse: (pipelineId) => ({
        selectedInputId: "input-primary",
        inputs: [
          {
            id: "input-primary",
            pipelineId,
            label: "Primary feed",
            streamKey: "sk_primary",
            role: "primary",
            enabled: true,
            selected: true,
            ingestUrls: { rtmp: "rtmp://seed/primary", srt: "srt://seed/primary" },
            previewUrl: "/hls/inputs/input-primary/master.m3u8",
            runtime: {
              connected: true,
              forwardingState: "active",
              protocol: "rtmp",
              bytesReceived: 12_582_912,
            },
          },
          {
            id: "input-standby",
            pipelineId,
            label: "Warm standby",
            streamKey: "sk_standby",
            role: "backup",
            enabled: true,
            selected: false,
            ingestUrls: { rtmp: "rtmp://seed/standby", srt: "srt://seed/standby" },
            previewUrl: "/hls/inputs/input-standby/master.m3u8",
            runtime: {
              connected: true,
              forwardingState: "standby",
              protocol: "srt",
              bytesReceived: 9_437_184,
            },
          },
        ],
      }),
    },
  );

  const primary = page.locator('article[data-card-id="input:input-primary"]');
  const standby = page.locator('article[data-card-id="input:input-standby"]');
  await expect(primary.getByRole("heading", { name: "Primary feed" })).toBeVisible();
  await expect(primary).toContainText("Forwarding");
  await expect(primary).toContainText("Selected · RTMP");
  await expect(standby.getByRole("heading", { name: "Warm standby" })).toBeVisible();
  await expect(standby).toContainText("Warm standby");
  await expect(standby).toContainText("Standby · SRT");

  await standby
    .locator('[data-action="control-room-toggle-card-actions"]')
    .click();
  await standby
    .getByRole("button", { name: "Promote", exact: true })
    .click();
  await expect(standby).toContainText("Forwarding");
  await expect(standby).toContainText("Selected · SRT");
  await expect(primary).toContainText("Connected standby");
  await expect(primary).toContainText("Standby · RTMP");
  expect(await getDocumentWidthOverflow(page)).toBe(0);

  await page.screenshot({
    path: testInfo.outputPath("multi-input-monitor.png"),
    fullPage: true,
  });
});

test("seed: default dashboard Monitor search understands operator status terms @desktop", async ({
  page,
}) => {
  await openSeededDashboard(
    page,
    "chaos-recovery",
    "/?mode=pipeline&view=monitor&p=pipe-retry-budget",
    {
      expectOverviewReady: false,
      settingsResponse: (settings) => ({
        ...settings,
        outputs: (settings.outputs as Array<Record<string, unknown>>).map(
          (output) =>
            output.pipelineId === "pipe-retry-budget"
              ? {
                  ...output,
                  monitoringUrl: `https://monitor.example.invalid/${String(output.id)}`,
                }
              : output,
        ),
      }),
    },
  );

  const monitor = page.locator("#control-mode-panel");
  const checkpoint = page.locator("#dashboard-v2-control-room-root");
  const search = monitor.locator("#control-room-search-input");
  const summary = monitor.locator("#control-room-summary");
  await expect(checkpoint).toContainText(
    "Monitoring Retry Budget Exhausted · 2 outputs · 2 monitors · 0 missing URLs",
  );
  await expect(checkpoint.getByText("2 monitors down")).toBeVisible();
  await expect(summary).toHaveText("2/2 monitored · 0 missing monitoring URLs");
  const buttonNames = await getCdpNamesByRole(page, "button");
  expect(buttonNames).toEqual(
    expect.arrayContaining([
      "Pause all monitor previews",
      "Reset monitor wall",
      "Show monitor actions for RTMP dead sink",
      "Show monitor actions for SRT dead sink",
    ]),
  );
  expect(buttonNames).not.toContain("Show monitor actions");
  expect(buttonNames).not.toContain("Pause All");
  expect(buttonNames).not.toContain("Reset");

  await search.fill("down");
  await expect(summary).toHaveText(
    '2/2 monitored match · 0 missing monitoring URLs · "down"',
  );
  await expect(
    monitor.getByText("RTMP dead sink", { exact: true }),
  ).toBeVisible();
  await expect(
    monitor.getByText("SRT dead sink", { exact: true }),
  ).toBeVisible();
  await expect(
    checkpoint.getByText('2/2 match "down"', { exact: true }),
  ).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    '2/2 monitored match · 0 missing monitoring URLs · "down"',
  );

  await search.fill("flapping");
  await expect(summary).toHaveText(
    '2/2 monitored match · 0 missing monitoring URLs · "flapping"',
  );
  await expect(
    checkpoint.getByText('2/2 match "flapping"', { exact: true }),
  ).toBeVisible();

  await search.fill("running");
  await expect(summary).toHaveText(
    '0/2 monitored match · 0 missing monitoring URLs · "running"',
  );
  await expect(
    monitor.getByText(
      'No monitoring outputs match "running". Clear search to show all monitoring cards.',
    ),
  ).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    '0/2 monitored match · 0 missing monitoring URLs · "running"',
  );
  expect(await getCdpNodeCount(page)).toBeLessThan(7_500);
});

test("seed: default dashboard Monitor lazily loads generic web previews @desktop", async ({
  page,
}) => {
  await openSeededDashboard(
    page,
    "chaos-recovery",
    "/?mode=pipeline&view=monitor&p=pipe-flapping",
    { expectOverviewReady: false },
  );

  const monitor = page.locator("#control-mode-panel");
  const checkpoint = page.locator("#dashboard-v2-control-room-root");
  await expect(checkpoint).toContainText(
    "Monitoring Recovered Sink Flap · 1 output · 1 monitor · 0 missing URLs",
  );
  await expect(
    checkpoint.locator("#dashboard-v2-control-room-title"),
  ).toBeVisible();
  await expect(
    checkpoint.getByText("1 lazy web preview", { exact: true }).first(),
  ).toBeVisible();
  await expect(
    monitor.getByRole("button", { name: "Load preview for SRT Sink Flap" }),
  ).toBeVisible();
  const outputCard = monitor.locator("article").filter({
    hasText: "SRT Sink Flap",
  });
  await expect(
    outputCard.getByRole("button", {
      name: "Edit monitoring URL for SRT Sink Flap",
    }),
  ).toBeHidden();
  await expect(
    outputCard.getByRole("button", {
      name: "Copy monitoring URL for SRT Sink Flap",
    }),
  ).toBeHidden();
  const showActions = outputCard.getByRole("button", {
    name: "Show monitor actions for SRT Sink Flap",
  });
  await expect(showActions).toBeVisible();
  await expect(showActions).toHaveAttribute("aria-expanded", "false");
  await showActions.click();
  await expect(
    outputCard.getByRole("button", {
      name: "Edit monitoring URL for SRT Sink Flap",
    }),
  ).toBeVisible();
  await expect(
    outputCard.getByRole("button", {
      name: "Copy monitoring URL for SRT Sink Flap",
    }),
  ).toBeVisible();
  await expect(
    outputCard.getByRole("button", { name: "Open monitor for SRT Sink Flap" }),
  ).toBeVisible();
  const hideActions = outputCard.getByRole("button", {
    name: "Hide monitor actions for SRT Sink Flap",
  });
  await expect(hideActions).toHaveAttribute("aria-expanded", "true");
  await hideActions.click();
  await expect(
    outputCard.getByRole("button", {
      name: "Edit monitoring URL for SRT Sink Flap",
    }),
  ).toBeHidden();
  await expect(monitor.locator("iframe")).toHaveCount(0);
  expect(await getCdpNodeCount(page)).toBeLessThan(12_000);

  await monitor
    .getByRole("button", { name: "Load preview for SRT Sink Flap" })
    .click();
  await expect(monitor.locator("iframe")).toHaveCount(1);
});

test("seed: default dashboard Monitor lazily loads HLS output previews @desktop", async ({
  page,
}) => {
  await openSeededDashboard(
    page,
    "mixed-health",
    "/?mode=pipeline&view=monitor&p=pipe-retrying",
    {
      expectOverviewReady: false,
      settingsResponse: (settings) => ({
        ...settings,
        outputs: (settings.outputs as Array<Record<string, unknown>>).map(
          (output) =>
            output.id === "out-retrying"
              ? {
                  ...output,
                  monitoringUrl:
                    "http://127.0.0.1:11888/live/out-retrying/index.m3u8",
                }
              : output,
        ),
      }),
      runtimeResponse: (runtime) => {
        const next = structuredClone(runtime);
        const output = (next.health as Record<string, Record<string, unknown>>)
          .pipelines?.["pipe-retrying"] as
          { outputs?: Record<string, Record<string, unknown>> } | undefined;
        if (output?.outputs?.["out-retrying"]) {
          Object.assign(output.outputs["out-retrying"], {
            status: "running",
            retrying: false,
            flapping: false,
            bitrateKbps: 900,
          });
        }
        return next;
      },
    },
  );

  const monitor = page.locator("#control-mode-panel");
  await expect(page.locator("#dashboard-v2-control-room-root")).toContainText(
    "Monitoring Retrying Destination · 1 output · 1 monitor · 0 missing URLs",
  );
  await expect(
    monitor.getByRole("button", { name: "Load preview for Retrying Output" }),
  ).toBeVisible();
  const outputCard = monitor.locator("article").filter({
    hasText: "Retrying Output",
  });
  await expect(
    outputCard.locator('[data-role="managed-hls-video"]'),
  ).toHaveCount(0);
  await expect(outputCard.locator("video")).toHaveCount(0);
  expect(await getCdpNodeCount(page)).toBeLessThan(7_500);

  await outputCard
    .getByRole("button", { name: "Load preview for Retrying Output" })
    .click();
  await expect(
    outputCard.locator('[data-role="managed-hls-video"]'),
  ).toHaveCount(1);
});
