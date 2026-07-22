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
  selectPipelineInV2Selector,
  tabUntilFocused,
} from "./seed-helpers";

test("seed: default v2 replaces Overview while delegating operator actions @desktop", async ({
  page,
}) => {
  await page.addInitScript(() => {
    const openedUrls: string[] = [];
    Object.defineProperty(window, "__redesignOpenedUrls", {
      configurable: true,
      value: openedUrls,
    });
    window.open = ((url?: string | URL) => {
      openedUrls.push(String(url));
      return null;
    }) as typeof window.open;
  });
  const pageErrors: string[] = [];
  const shellErrors: string[] = [];
  page.on("pageerror", (error) => pageErrors.push(error.message));
  page.on("console", (message) => {
    if (
      message.type() === "error" &&
      message.text().includes("Unable to start the dashboard v2 shell")
    ) {
      shellErrors.push(message.text());
    }
  });
  await openSeededDashboard(page, "mixed-health", "/?mode=overview", {
    outputControlDelayMs: 300,
    pipelineControlDelayMs: 300,
    settingsResponse: (settings) => ({
      ...settings,
      pipelines: (settings.pipelines as Array<Record<string, unknown>>).map(
        (pipeline) =>
          pipeline.id === "pipe-healthy"
            ? {
                ...pipeline,
                streamKey: "synthetic-healthy-stream-key-12345",
                ingestUrls: {
                  rtmp: "rtmp://ingest.example.invalid/live/synthetic-healthy-stream-key-12345",
                  srt: "srt://ingest.example.invalid:9000?streamid=synthetic-healthy-stream-key-12345",
                },
              }
            : pipeline.id === "pipe-retrying"
              ? {
                  ...pipeline,
                  inputSource: "file:synthetic-source.mp4",
                  fileIngest: {
                    configured: true,
                    id: "ingest-retrying",
                    filename: "synthetic-source.mp4",
                    loop: true,
                    running: false,
                  },
                }
              : pipeline,
      ),
    }),
    pipelineInputsResponse: (pipelineId) => ({
      selectedInputId: "input-primary",
      inputs: [
        {
          id: "input-primary",
          pipelineId,
          label: "Primary",
          streamKey: "synthetic-healthy-stream-key-12345",
          role: "primary",
          enabled: true,
          selected: true,
          ingestUrls: {
            rtmp:
              "rtmp://ingest.example.invalid/live/synthetic-healthy-stream-key-12345",
            srt: "srt://ingest.example.invalid:9000?streamid=synthetic-healthy-stream-key-12345",
          },
          previewUrl: "/hls/inputs/input-primary/master.m3u8",
          runtime: {
            connected: true,
            forwardingState: "active",
            protocol: "rtmp",
            uptimeSeconds: 720,
            bytesReceived: 48_000_000,
            remoteAddr: null,
            video: { codec: "h264", width: 1920, height: 1080 },
            audio: null,
            quality: null,
          },
        },
      ],
    }),
  });

  await expect(page).toHaveURL(/\?mode=overview$/);
  await page.waitForTimeout(100);
  expect(pageErrors).toEqual([]);
  expect(shellErrors).toEqual([]);
  expect(
    await page.evaluate(() => ({
      loaded: performance
        .getEntriesByType("resource")
        .some((entry) => entry.name.includes("dashboard-v2-entry.js")),
      rootHidden: document.getElementById("dashboard-v2-root")?.hidden,
      rootMarkup: document.getElementById("dashboard-v2-root")?.innerHTML,
      search: window.location.search,
    })),
  ).toEqual({
    loaded: true,
    rootHidden: false,
    rootMarkup: expect.stringContaining("dashboard-v2-overview"),
    search: "?mode=overview",
  });
  const v2Overview = page.locator("#dashboard-v2-overview");
  await expect(
    v2Overview.getByRole("heading", { name: "Fleet overview" }),
  ).toBeVisible();
  await expect(v2Overview).toContainText("1 pipeline needs attention");
  await expect(v2Overview).toContainText("Inputs live2/2");
  await expect(v2Overview).toContainText("Outputs running1/2");
  await expect(
    v2Overview.getByRole("button", {
      name: "Open pipeline Retrying Destination",
      exact: true,
    }),
  ).toBeVisible();
  await expect(
    v2Overview.getByRole("heading", { name: "Restream Activity" }),
  ).toBeVisible();
  await expect(page.locator("#overview-mode-content")).toBeHidden();
  expect(
    await page.locator("#overview-mode-content").evaluate((node) => ({
      childCount: node.childElementCount,
      text: node.textContent?.trim() ?? "",
    })),
  ).toEqual({ childCount: 0, text: "" });

  await v2Overview
    .getByRole("button", { name: "Add a new pipeline", exact: true })
    .click();
  await expect(page.locator("#edit-pipe-modal")).toBeVisible();
  await page.locator("#edit-pipe-modal").press("Escape");
  await v2Overview
    .getByRole("button", { name: "Operate Retrying Destination", exact: true })
    .click();
  await expect(page).toHaveURL(/mode=pipeline/);
  await expect(page.locator("#dashboard-grid")).toBeVisible();
  const pipelineSelector = page.locator("#dashboard-v2-pipeline-selector-root");
  await expect(pipelineSelector).toBeVisible();
  await expect(pipelineSelector.getByText("Pipelines")).toBeVisible();
  await expect(page.locator("#pipeline-selector-legacy")).toHaveCount(0);
  await expect(page.locator("#pipelines")).toHaveCount(0);
  await selectPipelineInV2Selector(
    pipelineSelector,
    "pipe-retrying",
    /Retrying Destination/,
  );
  await expect(pipelineSelector.getByLabel("Select pipeline")).toHaveValue(
    "pipe-retrying",
  );
  const pipelineHeader = page.locator("#dashboard-v2-pipeline-header-root");
  await expect(
    pipelineHeader.getByRole("heading", { name: "Retrying Destination" }),
  ).toBeVisible();
  await expect(pipelineHeader).toContainText("File · synthetic-source.mp4");
  const fileInputStatus = page.locator(
    "#dashboard-v2-pipeline-input-status-root",
  );
  await expect(fileInputStatus).toContainText("Source file");
  await expect(fileInputStatus).toContainText("synthetic-source.mp4");
  await expect(fileInputStatus).toContainText("MP4");
  await expect(fileInputStatus).toContainText("1.0 MiB");
  await expect(fileInputStatus).toContainText("Sparse source GOP detected");
  await expect(page.locator("#file-source-section")).toBeHidden();
  await expect(page.locator("#record-pipe-btn")).toBeHidden();
  await expect(page.locator("#file-ingest-pipe-btn")).toBeHidden();
  const startFile = pipelineHeader.getByRole("button", {
    name: "Start file ingest for Retrying Destination",
  });
  await startFile.click();
  await expect(
    pipelineHeader.getByRole("button", {
      name: "Starting file ingest for Retrying Destination",
    }),
  ).toBeDisabled();
  await expect(
    pipelineHeader.getByRole("button", {
      name: "Stop file ingest for Retrying Destination",
    }),
  ).toBeEnabled();
  await pipelineHeader
    .getByRole("button", {
      name: "Stop file ingest for Retrying Destination",
    })
    .click();
  await expect(
    pipelineHeader.getByRole("button", {
      name: "Stopping file ingest for Retrying Destination",
    }),
  ).toBeDisabled();
  await expect(startFile).toBeEnabled();
  await pipelineHeader
    .getByRole("button", {
      name: "Stop recording for Retrying Destination",
    })
    .click();
  await expect(
    pipelineHeader.getByRole("button", {
      name: "Stopping recording for Retrying Destination",
    }),
  ).toBeDisabled();
  await expect(
    pipelineHeader.getByRole("button", {
      name: "Start recording for Retrying Destination",
    }),
  ).toBeEnabled();
  await pipelineHeader
    .getByRole("button", {
      name: "Start recording for Retrying Destination",
    })
    .click();
  await expect(
    pipelineHeader.getByRole("button", {
      name: "Starting recording for Retrying Destination",
    }),
  ).toBeDisabled();
  await expect(
    pipelineHeader.getByRole("button", {
      name: "Stop recording for Retrying Destination",
    }),
  ).toBeEnabled();
  await expect(
    pipelineHeader.getByRole("button", {
      name: "Edit pipeline Retrying Destination",
    }),
  ).toBeDisabled();
  const outputOverview = page.locator(
    "#dashboard-v2-pipeline-output-overview-root",
  );
  await expect(outputOverview).toBeVisible();
  await expect(
    outputOverview.getByRole("heading", { name: "Output overview" }),
  ).toBeVisible();
  await expect(outputOverview).toContainText("Retrying");
  await expect(outputOverview).toContainText("Retrying Output");
  await expect(page.locator("#pipeline-output-overview-legacy")).toBeHidden();
  await expect(page.locator("#outs-col > h2")).toBeHidden();
  await expect(page.locator("#outputs-list")).toBeHidden();
  await expect(page.locator("#add-out-btn")).toBeHidden();
  const openRetryingOutputActions = async () => {
    await outputOverview
      .getByRole("button", { name: "More output actions for Retrying Output" })
      .click();
    await expect(
      outputOverview.getByRole("menu", {
        name: "More output actions for Retrying Output",
      }),
    ).toBeVisible();
  };
  await openRetryingOutputActions();
  await outputOverview
    .getByRole("menuitem", { name: "History Retrying Output" })
    .click();
  await expect(page.locator("#output-history-modal")).toBeVisible();
  await expect(page.locator("#output-history-title")).toHaveText(
    "History: Retrying Output",
  );
  await page.locator("#output-history-modal").press("Escape");
  await openRetryingOutputActions();
  await outputOverview
    .getByRole("menuitem", { name: "Monitor Retrying Output" })
    .click();
  expect(
    await page.evaluate(
      () =>
        (window as Window & { __redesignOpenedUrls?: string[] })
          .__redesignOpenedUrls,
    ),
  ).toEqual(["https://monitor.example.invalid/retrying"]);
  await openRetryingOutputActions();
  await outputOverview
    .getByRole("menuitem", { name: "Edit Retrying Output" })
    .click();
  await expect(page.locator("#edit-out-modal")).toBeVisible();
  await expect(page.locator("#out-modal-title")).toHaveText(
    'Edit Output "Retrying Output"',
  );
  await page.locator("#edit-out-modal").press("Escape");
  const stopRetryingOutput = outputOverview.getByRole("button", {
    name: "Stop Retrying Output",
  });
  await stopRetryingOutput.click();
  await expect(
    outputOverview.getByRole("button", { name: "Stopping Retrying Output" }),
  ).toBeDisabled();
  await expect(outputOverview).toContainText("Stopped");
  const startRetryingOutput = outputOverview.getByRole("button", {
    name: "Start Retrying Output",
  });
  await expect(startRetryingOutput).toBeEnabled();
  await openRetryingOutputActions();
  await expect(
    outputOverview.getByRole("menuitem", { name: "Delete Retrying Output" }),
  ).toBeEnabled();
  await outputOverview
    .getByRole("button", { name: "More output actions for Retrying Output" })
    .click();
  await expect(
    outputOverview.getByRole("menu", {
      name: "More output actions for Retrying Output",
    }),
  ).toBeHidden();
  await startRetryingOutput.click();
  await expect(
    outputOverview.getByRole("button", { name: "Starting Retrying Output" }),
  ).toBeDisabled();
  await expect(
    outputOverview.getByRole("button", { name: "Stop Retrying Output" }),
  ).toBeEnabled();
  await outputOverview
    .getByRole("button", { name: "Stop Retrying Output" })
    .click();
  await expect(
    outputOverview.getByRole("button", { name: "Start Retrying Output" }),
  ).toBeEnabled();
  await openRetryingOutputActions();
  await expect(
    outputOverview.getByRole("menuitem", { name: "Delete Retrying Output" }),
  ).toBeEnabled();
  await outputOverview
    .getByRole("menuitem", { name: "Delete Retrying Output" })
    .click();
  await expect(page.locator("#app-confirm-dialog")).toBeVisible();
  await expect(page.locator("#app-confirm-dialog")).toContainText(
    'Delete output "Retrying Output"?',
  );
  await page
    .locator("#app-confirm-dialog")
    .getByRole("button", { name: "Cancel" })
    .click();
  await outputOverview
    .getByRole("button", {
      name: "Add output for Retrying Destination",
      exact: true,
    })
    .click();
  await expect(page.locator("#edit-out-modal")).toBeVisible();
  await expect(page.locator("#out-modal-title")).toHaveText(
    'Add Output for "Retrying Destination"',
  );
  await page.locator("#edit-out-modal").press("Escape");

  await selectPipelineInV2Selector(
    pipelineSelector,
    "pipe-healthy",
    /Healthy Program/,
  );
  await expect(page).toHaveURL(/p=pipe-healthy/);
  await expect(page.locator("#pipe-name")).toHaveText("Healthy Program");
  await expect(pipelineHeader).toBeVisible();
  await expect(
    pipelineHeader.getByRole("heading", { name: "Healthy Program" }),
  ).toBeVisible();
  await expect(pipelineHeader).toContainText("RTMP");
  await expect(pipelineHeader).toContainText("Live");
  await expect(
    pipelineHeader.getByRole("button", {
      name: "Inspect graph for Healthy Program",
    }),
  ).toBeEnabled();
  await expect(
    pipelineHeader.getByRole("button", { name: "Diagnose Healthy Program" }),
  ).toBeEnabled();
  const healthyHeaderButtonNames = await getCdpNamesByRole(page, "button");
  expect(healthyHeaderButtonNames).toEqual(
    expect.arrayContaining([
      "Start recording for Healthy Program",
      "Inspect graph for Healthy Program",
      "Diagnose Healthy Program",
      "Edit pipeline Healthy Program",
    ]),
  );
  expect(healthyHeaderButtonNames).not.toEqual(
    expect.arrayContaining(["Record", "Graph", "Diagnose", "Edit"]),
  );
  expect(healthyHeaderButtonNames).not.toContain("Pipeline actions");
  await expect(page.locator("#pipeline-header-legacy-identity")).toBeHidden();
  await expect(page.locator("#graph-pipe-btn")).toBeHidden();
  await expect(page.locator("#diagnose-pipe-btn")).toBeHidden();
  await expect(page.locator("#edit-pipe-action-item")).toBeHidden();
  await expect(page.locator("#pipeline-header-legacy-actions")).toBeHidden();
  await expect(page.locator("#record-pipe-btn")).toBeHidden();
  const inputStatus = page.locator("#dashboard-v2-pipeline-input-status-root");
  await expect(inputStatus).toBeVisible();
  await expect(
    inputStatus.getByRole("heading", { name: "Input and preview" }),
  ).toBeVisible();
  await expect(inputStatus).toContainText("Live input");
  await expect(inputStatus).toContainText("Preview on demand");
  await expect(inputStatus).toContainText("H264 · 1920×1080");
  await expect(inputStatus).toContainText("1 audio track");
  await expect(inputStatus.getByText("Traffic", { exact: true })).toBeVisible();
  await expect(
    inputStatus.getByText("Input bitrate", { exact: true }),
  ).toBeVisible();
  await expect(inputStatus.getByText("Video", { exact: true })).toBeVisible();
  await expect(inputStatus).toContainText("1920×1080");
  await expect(page.locator("#publisher-meta")).toBeHidden();
  await expect(page.locator("#pipeline-input-legacy-traffic")).toBeHidden();
  await expect(page.locator("#pipeline-input-legacy-video")).toBeHidden();
  await expect(page.locator("#video-player")).toBeHidden();
  const previewPlayer = inputStatus.locator(
    '[data-role="dashboard-v2-input-preview"]',
  );
  await expect(previewPlayer).toBeVisible();
  await expect(
    previewPlayer.getByRole("button", {
      name: "Play input preview for Healthy Program",
    }),
  ).toBeVisible();
  expect(await getCdpNamesByRole(page, "button")).not.toContain("Play preview");
  await expect(page.locator("#input-stats")).toBeHidden();
  await expect(
    page.locator("#pipeline-input-legacy-audio-heading"),
  ).toBeHidden();
  await expect(page.locator("#input-audio-tracks")).toBeHidden();
  expect(
    await page.locator("#input-audio-tracks").evaluate((node) => ({
      childCount: node.childElementCount,
      text: node.textContent?.trim() ?? "",
    })),
  ).toEqual({ childCount: 0, text: "" });
  expect(
    await page.locator("#video-player").evaluate((node) => ({
      childCount: node.childElementCount,
      text: node.textContent?.trim() ?? "",
    })),
  ).toEqual({ childCount: 0, text: "" });
  expect(await getCdpNodeCount(page)).toBeLessThan(5_200);
  await expect(inputStatus.getByText("Audio", { exact: true })).toBeVisible();
  await expect(inputStatus.getByText("ENG", { exact: true })).toBeVisible();
  await inputStatus.getByRole("button", { name: "Rename ENG" }).click();
  const audioTrackName = inputStatus.getByRole("textbox", {
    name: "Audio track name",
  });
  await expect(audioTrackName).toBeFocused();
  await audioTrackName.fill("Program Audio");
  await audioTrackName.press("Enter");
  await expect(
    inputStatus.getByText("Program Audio", { exact: true }),
  ).toBeVisible();
  await inputStatus
    .getByRole("button", { name: "Rename Program Audio" })
    .click();
  const audioEditButtonNames = await getCdpNamesByRole(page, "button");
  expect(audioEditButtonNames).toEqual(
    expect.arrayContaining([
      "Save audio track Program Audio for Healthy Program",
      "Cancel audio track edit for Program Audio",
    ]),
  );
  expect(audioEditButtonNames).not.toEqual(
    expect.arrayContaining(["Save", "Cancel"]),
  );
  await audioTrackName.fill("Discarded label");
  await audioTrackName.press("Escape");
  await expect(
    inputStatus.getByText("Program Audio", { exact: true }),
  ).toBeVisible();
  await expect(inputStatus.getByText("Discarded label")).toHaveCount(0);
  await expect(page.locator("#stream-key-section")).toBeHidden();
  await expect(page.locator("#ingest-url-section")).toBeHidden();
  const primaryInput = inputStatus.locator("article").filter({
    has: page.getByRole("heading", { name: "Primary", exact: true }),
  });
  await expect(primaryInput).toContainText(
    "synthetic-healthy-stream-key-12345",
  );
  await expect(primaryInput).toContainText(
    "rtmp://ingest.example.invalid/live/synthetic-healthy-stream-key-12345",
  );
  await expect(primaryInput).toContainText(
    "srt://ingest.example.invalid:9000?streamid=synthetic-healthy-stream-key-12345",
  );
  await expect(
    primaryInput.getByRole("button", {
      name: "Copy SRT ingest URL for Primary",
    }),
  ).toBeVisible();
  const healthyInputButtonNames = await getCdpNamesByRole(page, "button");
  expect(healthyInputButtonNames).toEqual(
    expect.arrayContaining([
      "Copy stream key for Primary",
      "Copy RTMP ingest URL for Primary",
      "Copy SRT ingest URL for Primary",
    ]),
  );
  expect(healthyInputButtonNames).not.toEqual(
    expect.arrayContaining(["Copy", "Rename", "Promote"]),
  );
  await expect(outputOverview).toContainText("Running");
  await expect(outputOverview).toContainText("No outputs need attention");

  await pipelineHeader
    .getByRole("button", { name: "Edit pipeline Healthy Program" })
    .click();
  await expect(page.locator("#edit-pipe-modal")).toBeVisible();
  await expect(page.locator("#pipe-modal-title")).toHaveText("Edit Pipeline");
  await page.locator("#edit-pipe-modal").press("Escape");

  await pipelineSelector.getByRole("button", { name: "Add" }).click();
  await expect(page.locator("#edit-pipe-modal")).toBeVisible();
});

test("default v2 overview pipeline table supports large-fleet search @desktop", async ({
  page,
}) => {
  await page.goto("/login");
  await page.setContent(`
    <div id="dashboard-v2-root"></div>
    <div id="dashboard-v2-pipeline-selector-root"></div>
    <div id="dashboard-v2-pipeline-header-root"></div>
    <div id="dashboard-v2-pipeline-input-status-root"></div>
    <div id="dashboard-v2-pipeline-output-overview-root"></div>
    <div id="dashboard-v2-pipeline-inspect-root"></div>
    <div id="dashboard-v2-control-room-root"></div>
    <div id="dashboard-v2-incidents-root"></div>
    <div id="dashboard-v2-telemetry-root"></div>
    <div id="dashboard-v2-status-root"></div>
    <div id="dashboard-v2-media-root"></div>
    <div id="dashboard-v2-settings-root"></div>
  `);

  await page.evaluate(async () => {
    const importModule = new Function("path", "return import(path)") as (
      path: string,
    ) => Promise<{
      renderDashboardV2Overview: (
        model: Record<string, unknown>,
        actions: Record<string, unknown>,
      ) => void;
    }>;
    const { renderDashboardV2Overview } = await importModule(
      "/js/app/dashboard-v2-entry.js",
    );
    const neutral = { label: "--", tone: "neutral" };
    const rows = [
      {
        id: "pipe-main",
        name: "Main Program",
        health: { label: "Live", tone: "success", detail: "healthy" },
        input: { label: "Live", tone: "success", detail: "SRT ingest" },
        outputs: { label: "8/8 running", tone: "success" },
        inputRate: { label: "12.2 Mb/s", tone: "info" },
        outputRate: { label: "96.0 Mb/s", tone: "info" },
        recording: { label: "Off", tone: "neutral" },
      },
      {
        id: "pipe-ritual-backup",
        name: "Ritual backup",
        health: {
          label: "Output retrying",
          tone: "warning",
          detail: "recovering",
        },
        input: { label: "Live", tone: "success", detail: "RTMP ingest" },
        outputs: {
          label: "5/6 running",
          tone: "warning",
          detail: "1 retrying",
        },
        inputRate: { label: "10.8 Mb/s", tone: "info" },
        outputRate: { label: "54.0 Mb/s", tone: "info" },
        recording: { label: "Armed", tone: "warning", detail: "ready" },
      },
      ...Array.from({ length: 8 }, (_, index) => ({
        id: `pipe-side-${index}`,
        name: `Side Hall ${index + 1}`,
        health: { label: "Idle", tone: "neutral", detail: "waiting" },
        input: neutral,
        outputs: { label: "0/2 running", tone: "neutral" },
        inputRate: neutral,
        outputRate: neutral,
        recording: { label: "Off", tone: "neutral" },
      })),
    ];
    renderDashboardV2Overview(
      {
        counts: {
          pipelines: rows.length,
          liveInputs: 2,
          warningInputs: 1,
          outputs: 30,
          runningOutputs: 13,
          retryingOutputs: 1,
          flappingOutputs: 0,
          stoppedOutputs: 16,
          downOutputs: 0,
          recording: 0,
          inputKbps: 23_000,
          outputKbps: 150_000,
        },
        attentionPipelines: 1,
        attention: [],
        pipelines: rows,
        metrics: [],
        activity: [
          {
            headline: "Output retry burst",
            summary: "Ritual backup retried CDN destination twice.",
            details: ["warning", "Ritual backup", "cdn-primary"],
            eventCount: 2,
            startedAt: "2026-07-17T07:00:00Z",
            endedAt: "2026-07-17T07:01:00Z",
            tone: "warning",
          },
          {
            headline: "Input restored",
            summary:
              "Main Program SRT ingest recovered without operator action.",
            details: ["success", "Main Program", "srt"],
            eventCount: 3,
            startedAt: "2026-07-17T07:04:00Z",
            endedAt: "2026-07-17T07:05:00Z",
            tone: "success",
          },
          {
            headline: "Telemetry sample delayed",
            summary: "Engine metrics arrived late while the harness was busy.",
            details: ["neutral", "metrics", "engine"],
            eventCount: 1,
            startedAt: "2026-07-17T07:06:00Z",
            endedAt: "2026-07-17T07:07:00Z",
            tone: "neutral",
          },
        ],
        activityLoading: false,
      },
      {
        addPipeline: () => {},
        inspectPipeline: () => {},
        openPipeline: () => {},
        openStatus: () => {},
      },
    );
  });

  const overview = page.locator("#dashboard-v2-overview");
  await expect(overview.getByLabel("Search overview pipelines")).toBeVisible();
  await overview.getByLabel("Search overview pipelines").fill("ritual");
  await expect(
    overview.getByRole("button", { name: "Open pipeline Ritual backup" }),
  ).toHaveCount(2);
  await expect(
    overview.getByRole("button", { name: "Open pipeline Main Program" }),
  ).toHaveCount(0);
  const overviewClearSearch = overview.getByRole("button", {
    name: "Clear overview pipeline search",
  });
  await expect(overviewClearSearch).toBeVisible();
  const filteredOverviewButtonNames = await getCdpNamesByRole(page, "button");
  expect(filteredOverviewButtonNames).toContain(
    "Clear overview pipeline search",
  );
  expect(filteredOverviewButtonNames).toContain("Open pipeline Ritual backup");
  expect(filteredOverviewButtonNames).not.toContain("Ritual backup");
  expect(filteredOverviewButtonNames).not.toContain("Clear search");
  expect(await getCdpStatusTexts(page)).toContain(
    '1/10 pipelines shown · "ritual"',
  );

  await overviewClearSearch.click();
  await expect(overview.getByLabel("Search overview pipelines")).toHaveValue(
    "",
  );
  await expect(
    overview.getByRole("button", { name: "Open pipeline Main Program" }),
  ).toHaveCount(2);
  await expect(overviewClearSearch).toBeHidden();

  await overview.getByLabel("Search overview pipelines").fill("nowhere");
  await expect(overview.getByText("No pipelines match.").first()).toBeVisible();
  await expect(
    overview.getByText(
      'No overview pipelines match "nowhere". Clear search to show all.',
    ).first(),
  ).toBeVisible();
  expect(await getCdpStatusTexts(page)).toEqual(
    expect.arrayContaining([
      '0/10 pipelines shown · "nowhere"',
      'No overview pipelines match "nowhere". Clear search to show all.',
    ]),
  );

  await overviewClearSearch.click();
  await expect(
    overview.getByRole("button", { name: "Open pipeline Main Program" }),
  ).toHaveCount(2);
  await expect(
    overview.getByRole("button", { name: "Open pipeline Ritual backup" }),
  ).toHaveCount(2);

  await expect(overview.getByLabel("Search restream activity")).toBeVisible();
  await overview.getByLabel("Search restream activity").fill("restored");
  await expect(overview.getByText("Input restored")).toBeVisible();
  await expect(overview.getByText("Output retry burst")).not.toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    '1/3 bursts shown · "restored"',
  );
  const clearActivitySearch = overview.getByRole("button", {
    name: "Clear restream activity search",
  });
  await expect(clearActivitySearch).toBeVisible();
  const filteredActivityButtonNames = await getCdpNamesByRole(page, "button");
  expect(filteredActivityButtonNames).toEqual(
    expect.arrayContaining([
      "Open restream status",
      "Clear restream activity search",
    ]),
  );
  expect(filteredActivityButtonNames).not.toContain("Open Status");
  expect(filteredActivityButtonNames).not.toContain("Clear activity search");
  await clearActivitySearch.click();
  await expect(overview.getByLabel("Search restream activity")).toHaveValue("");
  await expect(overview.getByText("Output retry burst")).toBeVisible();

  await overview.getByLabel("Search restream activity").fill("missing");
  await expect(overview.getByText("No activity matches.")).toBeVisible();
  await expect(
    overview.getByText(
      'No restream activity matches "missing". Clear activity search to show all.',
    ),
  ).toBeVisible();
  expect(await getCdpStatusTexts(page)).toEqual(
    expect.arrayContaining([
      '0/3 bursts shown · "missing"',
      'No restream activity matches "missing". Clear activity search to show all.',
    ]),
  );

  expect(await getCdpNodeCount(page)).toBeLessThan(3_000);
});

test("default v2 output cards keep 125-output refreshes patch-only @desktop", async ({
  page,
}) => {
  await page.goto("/login");
  await page.setContent(`
    <div id="dashboard-v2-root"></div>
    <div id="dashboard-v2-pipeline-selector-root"></div>
    <div id="dashboard-v2-pipeline-header-root"></div>
    <div id="dashboard-v2-pipeline-input-status-root"></div>
    <div id="dashboard-v2-pipeline-output-overview-root"></div>
    <div id="dashboard-v2-pipeline-inspect-root"></div>
    <div id="dashboard-v2-control-room-root"></div>
    <div id="dashboard-v2-incidents-root"></div>
    <div id="dashboard-v2-telemetry-root"></div>
    <div id="dashboard-v2-status-root"></div>
    <div id="dashboard-v2-media-root"></div>
    <div id="dashboard-v2-settings-root"></div>
  `);

  const result = await page.evaluate(async () => {
    const importModule = new Function("path", "return import(path)") as (
      path: string,
    ) => Promise<{
      renderDashboardV2PipelineOutputOverview: (
        model: Record<string, unknown>,
        actions: Record<string, unknown>,
      ) => void;
    }>;
    const { renderDashboardV2PipelineOutputOverview } = await importModule(
      "/js/app/dashboard-v2-entry.js",
    );
    const actions = {
      addOutput: () => {},
      deleteOutput: async () => {},
      editOutput: () => {},
      monitorOutput: () => {},
      openOutputHistory: () => {},
      toggleOutput: async () => {},
      toggleOutputList: () => {},
    };
    const cards = Array.from({ length: 125 }, (_, index) => ({
      id: `out-${index}`,
      name: `Output ${index}`,
      urlLabel: `rtmp://example.invalid/live/output-${index}`,
      status: {
        label: "Running",
        tone: "success",
        detail: "Delivering media",
      },
      encodingLabel: "source",
      rateLabel: "1.5 Mb/s",
      uptimeLabel: "0:07:00",
      controlLabel: "Stop",
      controlDisabled: false,
      monitorAvailable: false,
      deleteDisabled: true,
    }));
    const frame = () =>
      new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    const model = (nextCards: typeof cards, expanded: boolean) => ({
      pipelineId: "pipe-bench",
      activeLabel: "125/125 active",
      aggregateRate: "187.5 Mb/s",
      counts: [
        { key: "running", label: "Running", tone: "success", count: 125 },
      ],
      attention: [],
      cards: expanded ? nextCards : nextCards.slice(0, 8),
      listCaption: expanded
        ? "Showing all 125 outputs"
        : "Showing first 8 of 125 outputs",
      expanded,
      canExpand: true,
    });
    const root = document.getElementById(
      "dashboard-v2-pipeline-output-overview-root",
    ) as HTMLElement;

    renderDashboardV2PipelineOutputOverview(model(cards, false), actions);
    await frame();
    await frame();
    const boundedCards = root.querySelectorAll("article").length;
    const collapsedButtons = Array.from(root.querySelectorAll("button"));
    const collapsedToggleLabel =
      collapsedButtons.at(-1)?.textContent?.trim() || "";
    renderDashboardV2PipelineOutputOverview(model(cards, true), actions);
    await frame();
    const expandedCards = root.querySelectorAll("article").length;
    const expandedButtons = Array.from(root.querySelectorAll("button"));
    const expandedToggleLabel =
      expandedButtons.at(-1)?.textContent?.trim() || "";

    const measure = async (mutate: boolean) => {
      const mutations = { attributes: 0, characterData: 0, childList: 0 };
      const observer = new MutationObserver((records) => {
        for (const record of records) {
          const target =
            record.target instanceof Element
              ? record.target
              : record.target.parentElement;
          if (!target?.closest("article")) continue;
          mutations[record.type] += 1;
        }
      });
      observer.observe(root, {
        attributes: true,
        characterData: true,
        childList: true,
        subtree: true,
      });
      let nextCards = cards;
      const startedAt = performance.now();
      for (let iteration = 0; iteration < 100; iteration += 1) {
        nextCards = nextCards.map((card) => ({
          ...card,
          rateLabel: mutate ? `${1501 + iteration} Kb/s` : card.rateLabel,
        }));
        renderDashboardV2PipelineOutputOverview(
          model(nextCards, true),
          actions,
        );
        await frame();
      }
      const durationMs = performance.now() - startedAt;
      observer.disconnect();
      return { durationMs, ...mutations };
    };

    return {
      boundedCards,
      collapsedToggleLabel,
      expandedCards,
      expandedToggleLabel,
      stable: await measure(false),
      live: await measure(true),
    };
  });

  console.log(`react-output-card-benchmark=${JSON.stringify(result)}`);
  expect(result.boundedCards).toBe(8);
  expect(result.collapsedToggleLabel).toBe("Show all 125");
  expect(result.expandedCards).toBe(125);
  expect(result.expandedToggleLabel).toBe("Show fewer");
  expect(result.stable).toMatchObject({
    attributes: 0,
    characterData: 0,
    childList: 0,
  });
  expect(result.live.characterData).toBeGreaterThan(0);
  expect(result.live.childList).toBe(0);
});

test("default v2 pipeline selector supports search under long lists @desktop", async ({
  page,
}) => {
  await page.goto("/login");
  await page.setContent(`
    <div id="dashboard-v2-root"></div>
    <div id="dashboard-v2-pipeline-selector-root"></div>
    <div id="dashboard-v2-pipeline-header-root"></div>
    <div id="dashboard-v2-pipeline-input-status-root"></div>
    <div id="dashboard-v2-pipeline-output-overview-root"></div>
    <div id="dashboard-v2-pipeline-inspect-root"></div>
    <div id="dashboard-v2-control-room-root"></div>
    <div id="dashboard-v2-incidents-root"></div>
    <div id="dashboard-v2-telemetry-root"></div>
    <div id="dashboard-v2-status-root"></div>
    <div id="dashboard-v2-media-root"></div>
    <div id="dashboard-v2-settings-root"></div>
  `);

  await page.evaluate(async () => {
    const importModule = new Function("path", "return import(path)") as (
      path: string,
    ) => Promise<{
      renderDashboardV2PipelineSelector: (
        model: Record<string, unknown>,
        actions: Record<string, unknown>,
      ) => void;
    }>;
    const { renderDashboardV2PipelineSelector } = await importModule(
      "/js/app/dashboard-v2-entry.js",
    );
    renderDashboardV2PipelineSelector(
      {
        selectedPipelineId: "pipe-ritual-backup",
        pipelines: [
          {
            id: "pipe-main",
            name: "Main Program",
            statusTone: "success",
            statusLabel: "Live",
            runningOutputs: 8,
            totalOutputs: 8,
            inputRate: "12.2 Mb/s",
            outputRate: "96.0 Mb/s",
            selected: false,
          },
          {
            id: "pipe-ritual-backup",
            name: "Ritual backup",
            statusTone: "warning",
            statusLabel: "Retrying",
            runningOutputs: 5,
            totalOutputs: 6,
            inputRate: "10.8 Mb/s",
            outputRate: "54.0 Mb/s",
            selected: true,
          },
          ...Array.from({ length: 8 }, (_, index) => ({
            id: `pipe-side-${index}`,
            name: `Side Hall ${index + 1}`,
            statusTone: "neutral",
            statusLabel: "Idle",
            runningOutputs: 0,
            totalOutputs: 2,
            inputRate: "--",
            outputRate: "--",
            selected: false,
          })),
        ],
      },
      {
        addPipeline: () => {},
        selectPipeline: () => {},
      },
    );
  });

  const selector = page.locator("#dashboard-v2-pipeline-selector-root");
  await expect(selector.getByLabel("Search pipelines")).toBeVisible();
  await selector.getByLabel("Search pipelines").fill("backup");
  await expect(
    selector.getByRole("button", { name: "Select pipeline Ritual backup" }),
  ).toBeVisible();
  await expect(
    selector.getByRole("button", { name: "Select pipeline Ritual backup" }),
  ).toHaveAttribute("aria-current", "page");
  await expect(
    selector.getByRole("button", { name: "Select pipeline Main Program" }),
  ).not.toBeVisible();
  const selectorClearSearch = selector.getByRole("button", {
    name: "Clear pipeline selector search",
  });
  await expect(selectorClearSearch).toBeVisible();
  const filteredSelectorButtonNames = await getCdpNamesByRole(page, "button");
  expect(filteredSelectorButtonNames).toContain(
    "Clear pipeline selector search",
  );
  expect(filteredSelectorButtonNames).not.toContain("Clear search");
  expect(await getCdpStatusTexts(page)).toContain(
    '1/10 pipelines shown · "backup"',
  );

  await selectorClearSearch.click();
  await expect(selector.getByLabel("Search pipelines")).toHaveValue("");
  await expect(
    selector.getByRole("button", { name: "Select pipeline Main Program" }),
  ).toBeVisible();
  await expect(selectorClearSearch).toBeHidden();

  await selector.getByLabel("Search pipelines").fill("nowhere");
  await expect(selector.getByText("No pipelines match.")).toBeVisible();
  await expect(
    selector.getByText(
      'No pipelines match "nowhere". Clear search to return to all pipelines.',
    ),
  ).toBeVisible();
  expect(await getCdpStatusTexts(page)).toEqual(
    expect.arrayContaining([
      '0/10 pipelines shown · "nowhere"',
      'No pipelines match "nowhere". Clear search to return to all pipelines.',
    ]),
  );

  await selectorClearSearch.click();
  await expect(
    selector.getByRole("button", { name: "Select pipeline Main Program" }),
  ).toBeVisible();
  await expect(
    selector.getByRole("button", { name: "Select pipeline Ritual backup" }),
  ).toBeVisible();
  const unfilteredSelectorButtonNames = await getCdpNamesByRole(page, "button");
  expect(unfilteredSelectorButtonNames).toEqual(
    expect.arrayContaining([
      "Select pipeline Main Program",
      "Select pipeline Ritual backup",
    ]),
  );
  expect(unfilteredSelectorButtonNames).not.toEqual(
    expect.arrayContaining([
      "Main ProgramLive · 8/8 outputs12.2 Mb/s in96.0 Mb/s out",
      "Ritual backupStandby · 1/2 outputs3.0 Mb/s in1.5 Mb/s out",
    ]),
  );
});

test("default v2 pipeline details placeholder makes convergence explicit @desktop", async ({
  page,
}) => {
  await page.goto("/login");
  await page.setContent(`
    <div id="dashboard-v2-root"></div>
    <div id="dashboard-v2-pipeline-selector-root"></div>
    <div id="dashboard-v2-pipeline-header-root"></div>
    <div id="dashboard-v2-pipeline-input-status-root"></div>
    <div id="dashboard-v2-pipeline-output-overview-root"></div>
    <div id="dashboard-v2-pipeline-inspect-root"></div>
    <div id="dashboard-v2-control-room-root"></div>
    <div id="dashboard-v2-incidents-root"></div>
    <div id="dashboard-v2-telemetry-root"></div>
    <div id="dashboard-v2-status-root"></div>
    <div id="dashboard-v2-media-root"></div>
    <div id="dashboard-v2-settings-root"></div>
  `);

  await page.evaluate(async () => {
    const importModule = new Function("path", "return import(path)") as (
      path: string,
    ) => Promise<{
      renderDashboardV2PipelineHeader: (
        model: Record<string, unknown> | null,
        actions: Record<string, unknown>,
        placeholder?: Record<string, unknown> | null,
      ) => void;
    }>;
    const { renderDashboardV2PipelineHeader } = await importModule(
      "/js/app/dashboard-v2-entry.js",
    );
    const actions = {
      diagnosePipeline: () => {},
      editPipeline: () => {},
      inspectPipeline: () => {},
      toggleFileIngest: async () => {},
      toggleRecording: async () => {},
    };
    renderDashboardV2PipelineHeader(null, actions, {
      title: "Loading pipeline details",
      message:
        "The selected pipeline is catching up with the latest runtime snapshot.",
    });
  });

  const header = page.locator("#dashboard-v2-pipeline-header-root");
  await expect(header).toBeVisible();
  await expect(
    header.getByRole("heading", { name: "Loading pipeline details" }),
  ).toBeVisible();
  await expect(header).toContainText("catching up");
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Performance.enable");
  const performanceMetrics = await cdp.send("Performance.getMetrics");
  const nodeMetric = performanceMetrics.metrics.find(
    (metric) => metric.name === "Nodes",
  );
  expect(nodeMetric?.value ?? 0).toBeLessThan(1_000);
  await cdp.detach();

  await page.evaluate(async () => {
    const importModule = new Function("path", "return import(path)") as (
      path: string,
    ) => Promise<{
      renderDashboardV2PipelineHeader: (
        model: Record<string, unknown> | null,
        actions: Record<string, unknown>,
        placeholder?: Record<string, unknown> | null,
      ) => void;
    }>;
    const { renderDashboardV2PipelineHeader } = await importModule(
      "/js/app/dashboard-v2-entry.js",
    );
    renderDashboardV2PipelineHeader(null, {
      diagnosePipeline: () => {},
      editPipeline: () => {},
      inspectPipeline: () => {},
      toggleFileIngest: async () => {},
      toggleRecording: async () => {},
    });
  });
  await expect(header).toBeHidden();
});

test("default v2 keeps failed recording mutation context in the pipeline header @desktop", async ({
  page,
}) => {
  await openSeededDashboard(
    page,
    "mixed-health",
    "/?mode=pipeline&view=operate&p=pipe-healthy",
    {
      expectOverviewReady: false,
      failRecordingControl: "recording target disk is full",
      pipelineControlDelayMs: 250,
    },
  );

  const header = page.locator("#dashboard-v2-pipeline-header-root");
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Performance.enable");
  await cdp.send("Performance.getMetrics");

  await header
    .getByRole("button", { name: "Start recording for Healthy Program" })
    .click();
  await expect(
    header.getByRole("button", {
      name: "Starting recording for Healthy Program",
    }),
  ).toBeDisabled();
  await expect(
    header.getByRole("status").filter({
      hasText: "Recording request failed",
    }),
  ).toBeVisible();
  await expect(header).toContainText("Start recording did not complete");
  await expect(
    header.getByRole("button", {
      name: "Start recording for Healthy Program",
    }),
  ).toBeEnabled();
  await expect(page.locator("#error-alert")).toContainText(
    "recording target disk is full",
  );

  const afterMetrics = await cdp.send("Performance.getMetrics");
  const afterNodes =
    afterMetrics.metrics.find((metric) => metric.name === "Nodes")?.value ?? 0;
  expect(afterNodes).toBeLessThan(7_000);
  await cdp.detach();
});

test("default v2 keeps failed file-ingest mutation context in the pipeline header @desktop", async ({
  page,
}) => {
  await openSeededDashboard(
    page,
    "mixed-health",
    "/?mode=pipeline&view=operate&p=pipe-retrying",
    {
      expectOverviewReady: false,
      failFileIngestControl: "file source disappeared before ingest start",
      pipelineControlDelayMs: 250,
      settingsResponse: (settings) => ({
        ...settings,
        pipelines: (settings.pipelines as Array<Record<string, unknown>>).map(
          (pipeline) =>
            pipeline.id === "pipe-retrying"
              ? {
                  ...pipeline,
                  inputSource: "file:synthetic-source.mp4",
                  fileIngest: {
                    configured: true,
                    id: "ingest-retrying",
                    filename: "synthetic-source.mp4",
                    loop: true,
                    running: false,
                  },
                }
              : pipeline,
        ),
      }),
    },
  );

  const header = page.locator("#dashboard-v2-pipeline-header-root");
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Performance.enable");

  await header
    .getByRole("button", {
      name: "Start file ingest for Retrying Destination",
    })
    .click();
  await expect(
    header.getByRole("button", {
      name: "Starting file ingest for Retrying Destination",
    }),
  ).toBeDisabled();
  await expect(
    header.getByRole("status").filter({
      hasText: "File ingest request failed",
    }),
  ).toBeVisible();
  await expect(header).toContainText("Start file ingest did not complete");
  await expect(
    header.getByRole("button", {
      name: "Start file ingest for Retrying Destination",
    }),
  ).toBeEnabled();
  await expect(page.locator("#error-alert")).toContainText(
    "file source disappeared before ingest start",
  );

  const metrics = await cdp.send("Performance.getMetrics");
  const nodes =
    metrics.metrics.find((metric) => metric.name === "Nodes")?.value ?? 0;
  expect(nodes).toBeLessThan(7_000);
  await cdp.detach();
});

test("default v2 keeps failed output mutation context on the output card @desktop", async ({
  page,
}) => {
  await openSeededDashboard(
    page,
    "mixed-health",
    "/?mode=pipeline&view=operate&p=pipe-healthy",
    {
      expectOverviewReady: false,
      failOutputControl: "destination refused stop command",
      outputControlDelayMs: 250,
    },
  );

  const outputOverview = page.locator(
    "#dashboard-v2-pipeline-output-overview-root",
  );
  const card = outputOverview
    .locator("article")
    .filter({ hasText: "Healthy Output" });
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Performance.enable");

  await card.getByRole("button", { name: "Stop Healthy Output" }).click();
  await expect(
    card.getByRole("button", { name: "Stopping Healthy Output" }),
  ).toBeDisabled();
  await expect(
    card.getByRole("status").filter({
      hasText: "Output request failed",
    }),
  ).toBeVisible();
  await expect(card).toContainText("Stop output did not complete");
  await expect(
    card.getByRole("button", { name: "Stop Healthy Output" }),
  ).toBeEnabled();
  await expect(page.locator("#error-alert")).toContainText(
    "destination refused stop command",
  );

  const metrics = await cdp.send("Performance.getMetrics");
  const nodes =
    metrics.metrics.find((metric) => metric.name === "Nodes")?.value ?? 0;
  expect(nodes).toBeLessThan(7_000);
  await cdp.detach();
});

test("default v2 output action menus are keyboard-dismissable @desktop", async ({
  page,
}) => {
  await openSeededDashboard(
    page,
    "mixed-health",
    "/?mode=pipeline&view=operate&p=pipe-healthy",
    { expectOverviewReady: false },
  );

  const outputOverview = page.locator(
    "#dashboard-v2-pipeline-output-overview-root",
  );
  const more = outputOverview.getByRole("button", {
    name: "More output actions for Healthy Output",
  });
  await more.focus();
  await page.keyboard.press("Enter");
  const menu = outputOverview.getByRole("menu", {
    name: "More output actions for Healthy Output",
  });
  await expect(menu).toBeVisible();
  await expect(more).toHaveAttribute("aria-expanded", "true");

  const cdp = await page.context().newCDPSession(page);
  const axTree = await cdp.send("Accessibility.getFullAXTree");
  const menuRoles = axTree.nodes
    .filter(
      (node) => node.name?.value === "More output actions for Healthy Output",
    )
    .map((node) => node.role?.value);
  expect(menuRoles).toContain("menu");
  expect(
    axTree.nodes.some(
      (node) => node.name?.value === "More actions for Healthy Output",
    ),
  ).toBe(false);
  expect(
    axTree.nodes.some(
      (node) =>
        node.role?.value === "menuitem" &&
        node.name?.value === "History Healthy Output",
    ),
  ).toBe(true);
  await cdp.detach();

  await page.keyboard.press("Tab");
  await expect(
    outputOverview.getByRole("menuitem", { name: "History Healthy Output" }),
  ).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(menu).toBeHidden();
  await expect(more).toBeFocused();
  await expect(more).toHaveAttribute("aria-expanded", "false");
});

test("default v2 output destinations support search and state filters @desktop", async ({
  page,
}) => {
  await page.goto("/login");
  await page.setContent(`
    <div id="dashboard-v2-root"></div>
    <div id="dashboard-v2-pipeline-selector-root"></div>
    <div id="dashboard-v2-pipeline-header-root"></div>
    <div id="dashboard-v2-pipeline-input-status-root"></div>
    <div id="dashboard-v2-pipeline-output-overview-root"></div>
    <div id="dashboard-v2-pipeline-inspect-root"></div>
    <div id="dashboard-v2-control-room-root"></div>
    <div id="dashboard-v2-incidents-root"></div>
    <div id="dashboard-v2-telemetry-root"></div>
    <div id="dashboard-v2-status-root"></div>
    <div id="dashboard-v2-media-root"></div>
    <div id="dashboard-v2-settings-root"></div>
  `);

  await page.evaluate(async () => {
    const importModule = new Function("path", "return import(path)") as (
      path: string,
    ) => Promise<{
      renderDashboardV2PipelineOutputOverview: (
        model: Record<string, unknown>,
        actions: Record<string, unknown>,
      ) => void;
    }>;
    const { renderDashboardV2PipelineOutputOverview } = await importModule(
      "/js/app/dashboard-v2-entry.js",
    );
    const actions = {
      addOutput: () => {},
      deleteOutput: async () => {},
      editOutput: () => {},
      monitorOutput: () => {},
      openOutputHistory: () => {},
      toggleOutput: async () => {},
      toggleOutputList: () => {},
    };
    renderDashboardV2PipelineOutputOverview(
      {
        pipelineId: "pipe-filter",
        activeLabel: "1/5 active",
        aggregateRate: "1.5 Mb/s",
        counts: [
          { key: "running", label: "Running", tone: "success", count: 1 },
          { key: "retrying", label: "Retrying", tone: "warning", count: 1 },
          { key: "stopped", label: "Stopped", tone: "neutral", count: 1 },
        ],
        attention: [],
        cards: [
          {
            id: "primary",
            name: "YouTube primary",
            urlLabel: "rtmp://example.invalid/live/youtube",
            status: {
              label: "Running",
              tone: "success",
              detail: "Delivering media",
            },
            encodingLabel: "source",
            rateLabel: "1.5 Mb/s",
            uptimeLabel: "0:07:00",
            controlLabel: "Stop",
            controlDisabled: false,
            monitorAvailable: false,
            deleteDisabled: true,
          },
          {
            id: "backup",
            name: "Facebook backup",
            urlLabel: "rtmp://example.invalid/live/facebook",
            status: {
              label: "Retrying",
              tone: "warning",
              detail: "Retry in 6s",
            },
            encodingLabel: "720p",
            rateLabel: "--",
            uptimeLabel: null,
            controlLabel: "Stop",
            controlDisabled: false,
            monitorAvailable: true,
            deleteDisabled: true,
          },
          {
            id: "archive",
            name: "Archive",
            urlLabel: "rtmp://example.invalid/live/archive",
            status: {
              label: "Stopped",
              tone: "neutral",
              detail: "Stopped by operator",
            },
            encodingLabel: "source",
            rateLabel: "--",
            uptimeLabel: null,
            controlLabel: "Start",
            controlDisabled: false,
            monitorAvailable: false,
            deleteDisabled: false,
          },
          {
            id: "led-wall",
            name: "LED wall",
            urlLabel: "srt://example.invalid/live/led-wall",
            status: {
              label: "Stopped",
              tone: "neutral",
              detail: "Stopped by operator",
            },
            encodingLabel: "source",
            rateLabel: "--",
            uptimeLabel: null,
            controlLabel: "Start",
            controlDisabled: false,
            monitorAvailable: false,
            deleteDisabled: false,
          },
          {
            id: "hls-preview",
            name: "Internal HLS preview",
            urlLabel: "hls://preview",
            status: {
              label: "Stopped",
              tone: "neutral",
              detail: "Stopped by operator",
            },
            encodingLabel: "source",
            rateLabel: "--",
            uptimeLabel: null,
            controlLabel: "Start",
            controlDisabled: false,
            monitorAvailable: false,
            deleteDisabled: false,
          },
        ],
        listCaption: null,
        expanded: false,
        canExpand: false,
      },
      actions,
    );
  });

  const root = page.locator("#dashboard-v2-pipeline-output-overview-root");
  await expect(
    root.getByRole("heading", { name: "YouTube primary" }),
  ).toBeVisible();
  await expect(
    root.getByRole("heading", { name: "Facebook backup" }),
  ).toBeVisible();
  await expect(root.getByRole("heading", { name: "Archive" })).toBeVisible();

  await root.getByLabel("Search output destinations").fill("facebook");
  await expect(
    root.getByRole("heading", { name: "Facebook backup" }),
  ).toBeVisible();
  await expect(
    root.getByRole("heading", { name: "YouTube primary" }),
  ).not.toBeVisible();
  await expect(root.getByText("1/5 shown")).toBeVisible();
  const clearActiveFilters = root.getByRole("button", {
    name: "Clear output destination filters",
  });
  await expect(clearActiveFilters).toBeVisible();
  let outputButtonNames = await getCdpNamesByRole(page, "button");
  expect(outputButtonNames).toEqual(
    expect.arrayContaining([
      "Show all output destinations",
      "Show running output destinations",
      "Show stopped output destinations",
      "Clear output destination filters",
    ]),
  );
  expect(outputButtonNames).not.toContain("All");
  expect(outputButtonNames).not.toContain("Stopped");
  expect(outputButtonNames).not.toContain("Clear output filters");
  expect(await getCdpStatusTexts(page)).toContain(
    '1/5 shown · All · "facebook"',
  );

  await clearActiveFilters.click();
  await expect(root.getByLabel("Search output destinations")).toHaveValue("");
  await expect(
    root.getByRole("heading", { name: "YouTube primary" }),
  ).toBeVisible();
  await expect(
    root.getByRole("heading", { name: "Facebook backup" }),
  ).toBeVisible();
  await expect(clearActiveFilters).toBeHidden();

  await root.getByLabel("Search output destinations").fill("facebook");
  await root
    .getByRole("button", { name: "Show stopped output destinations" })
    .click();
  await expect(root.getByText("No outputs match.")).toBeVisible();
  await expect(
    root.getByText(
      'No stopped output destinations match "facebook". Clear filters to show all.',
    ),
  ).toBeVisible();

  const cdp = await page.context().newCDPSession(page);
  const axTree = await cdp.send("Accessibility.getFullAXTree");
  const axNodeById = new Map(axTree.nodes.map((node) => [node.nodeId, node]));
  const statusTexts = axTree.nodes
    .filter((node) => node.role?.value === "status")
    .map((node) =>
      (node.childIds ?? [])
        .map((childId) => axNodeById.get(childId)?.name?.value)
        .filter(Boolean)
        .join(""),
    );
  expect(statusTexts).toContain('0/5 shown · Stopped · "facebook"');
  expect(statusTexts).toContain(
    'No stopped output destinations match "facebook". Clear filters to show all.',
  );
  outputButtonNames = axTree.nodes
    .filter((node) => node.role?.value === "button")
    .map((node) => String(node.name?.value ?? ""))
    .filter(Boolean);
  expect(outputButtonNames).toContain("Clear output destination filters");
  expect(outputButtonNames).toContain(
    "Clear no-result output destination filters",
  );
  expect(
    outputButtonNames.filter(
      (name) => name === "Clear output destination filters",
    ),
  ).toHaveLength(1);
  expect(outputButtonNames).not.toContain("Clear output filters");
  await cdp.detach();

  await root
    .getByRole("button", { name: "Clear output destination filters" })
    .click();
  await expect(
    root.getByRole("heading", { name: "YouTube primary" }),
  ).toBeVisible();
  await expect(
    root.getByRole("heading", { name: "Facebook backup" }),
  ).toBeVisible();
  await expect(root.getByRole("heading", { name: "Archive" })).toBeVisible();

  await root
    .getByRole("button", { name: "Show attention output destinations" })
    .click();
  await expect(
    root.getByRole("heading", { name: "Facebook backup" }),
  ).toBeVisible();
  await expect(
    root.getByRole("heading", { name: "YouTube primary" }),
  ).not.toBeVisible();
  await expect(
    root.getByRole("heading", { name: "Archive" }),
  ).not.toBeVisible();
});
