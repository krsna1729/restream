import { expect, test } from "@playwright/test";

import { openSeededDashboard } from "./fixtures";

test("seed: empty Overview is deterministic and canonical @desktop", async ({
  page,
}) => {
  await openSeededDashboard(page, "empty");

  const overview = page.locator("#overview-mode-content");
  await expect(page).toHaveURL(/\?mode=overview$/);
  await expect(
    overview.getByRole("cell", { name: "No pipelines configured." }),
  ).toBeVisible();
  await expect(
    overview.getByRole("button", { name: "Add Pipeline", exact: true }),
  ).toBeVisible();
  await expect(page.locator("#dashboard-v2-root")).toBeHidden();
  await expect(
    page.locator("#dashboard-v2-pipeline-selector-root"),
  ).toBeHidden();
  await expect(page.locator("#dashboard-v2-pipeline-header-root")).toBeHidden();
  await expect(
    page.locator("#dashboard-v2-pipeline-input-status-root"),
  ).toBeHidden();
  await expect(
    page.locator("#dashboard-v2-pipeline-output-overview-root"),
  ).toBeHidden();
  await expect(page.locator("#pipeline-selector-legacy")).not.toHaveAttribute(
    "hidden",
  );
  await expect(
    page.locator("#pipeline-header-legacy-identity"),
  ).not.toHaveAttribute("hidden");
  await expect(
    page.locator("#pipeline-output-overview-legacy"),
  ).not.toHaveAttribute("hidden");
  await expect(page.locator("#outs-col > h2")).not.toHaveAttribute("hidden");
  expect(
    await page.evaluate(() =>
      performance
        .getEntriesByType("resource")
        .some((entry) => entry.name.includes("dashboard-v2-entry.js")),
    ),
  ).toBe(false);
});

test("seed: mixed-health Overview exposes upstream and output state @desktop", async ({
  page,
}) => {
  await openSeededDashboard(page, "mixed-health");

  const overview = page.locator("#overview-mode-content");
  await expect(
    overview.getByRole("button", { name: "Healthy Program", exact: true }),
  ).toBeVisible();
  await expect(
    overview.getByRole("button", {
      name: "Retrying Destination",
      exact: true,
    }),
  ).toBeVisible();
  const attention = overview.locator("#overview-attention");
  await expect(attention).toContainText("1 pipeline needs attention");
  await expect(
    attention.getByRole("heading", { name: "Retrying Destination" }),
  ).toBeVisible();
  await expect(
    attention.getByRole("heading", { name: "Healthy Program" }),
  ).toHaveCount(0);
  await expect(page.locator("#workspace-mode-summary")).toContainText(
    "1 retrying",
  );
});

test("seed: ui=v2 keeps legacy-owned routes off the React seam @desktop", async ({
  page,
}) => {
  const v2Requests: string[] = [];
  page.on("request", (request) => {
    if (request.url().includes("dashboard-v2-entry.js")) {
      v2Requests.push(request.url());
    }
  });

  await openSeededDashboard(page, "mixed-health", "/?mode=settings&ui=v2", {
    expectOverviewReady: false,
  });
  await expect(page).toHaveURL(/\?mode=settings&ui=v2$/);
  await expect(
    page.locator("#settings-mode-content").getByRole("heading", {
      name: "Settings",
    }),
  ).toBeVisible();
  await expect(page.locator("#dashboard-v2-root")).toBeHidden();
  await expect(
    page.locator("#dashboard-v2-pipeline-selector-root"),
  ).toBeHidden();
  expect(v2Requests).toEqual([]);

  await page.goto("/?mode=status&ui=v2");
  await expect(
    page.locator("#status-mode-content").getByRole("heading", {
      name: "Status",
    }),
  ).toBeVisible();
  await expect(page.locator("#status-versions")).toContainText("seeded");
  await expect(page.locator("#dashboard-v2-root")).toBeHidden();
  await expect(
    page.locator("#dashboard-v2-pipeline-selector-root"),
  ).toBeHidden();
  expect(v2Requests).toEqual([]);

  await page.goto("/?mode=pipeline&view=inspect&p=pipe-healthy&ui=v2");
  await expect(page.locator("#inspect-mode-panel")).toBeVisible();
  await expect(
    page.locator("#dashboard-v2-pipeline-selector-root"),
  ).toBeHidden();
  expect(v2Requests).toEqual([]);

  await page.goto("/?mode=pipeline&view=monitor&p=pipe-healthy&ui=v2");
  await expect(page.locator("#control-mode-panel")).toBeVisible();
  await expect(
    page.locator("#dashboard-v2-pipeline-selector-root"),
  ).toBeHidden();
  expect(v2Requests).toEqual([]);

  await page.goto("/?mode=pipeline&view=operate&ui=v2");
  await expect(
    page
      .locator("#dashboard-v2-pipeline-selector-root")
      .getByRole("heading", { name: "Pipelines" }),
  ).toBeVisible();
  expect(v2Requests.length).toBe(1);
});

test("seed: ui=v2 surfaces harness-derived chaos recovery states @desktop", async ({
  page,
}) => {
  await openSeededDashboard(
    page,
    "chaos-recovery",
    "/?mode=pipeline&view=operate&p=pipe-grace&ui=v2",
    { expectOverviewReady: false },
  );

  const pipelineSelector = page.locator("#dashboard-v2-pipeline-selector-root");
  const pipelineHeader = page.locator("#dashboard-v2-pipeline-header-root");
  const inputStatus = page.locator("#dashboard-v2-pipeline-input-status-root");
  const outputOverview = page.locator(
    "#dashboard-v2-pipeline-output-overview-root",
  );

  await expect(
    pipelineSelector.getByRole("heading", { name: "Pipelines" }),
  ).toBeVisible();
  await expect(
    pipelineHeader.getByRole("heading", { name: "Transient Publisher Drop" }),
  ).toBeVisible();
  await expect(inputStatus).toContainText("Reconnecting");
  await expect(inputStatus).toContainText("Disconnect grace active");
  await expect(outputOverview).toContainText("Grace-preserved Output");
  await expect(outputOverview).toContainText("Running");

  await page.goto(
    "/?mode=pipeline&view=operate&p=pipe-hls-timeout&ui=v2",
  );
  await expect(
    pipelineHeader.getByRole("heading", { name: "HLS Timeout Recovery" }),
  ).toBeVisible();
  await expect(outputOverview).toContainText("HLS PUT Sink");
  await expect(outputOverview).toContainText("Retrying");
  await expect(outputOverview).toContainText("Retry in 8s");

  await page.goto("/?mode=pipeline&view=operate&p=pipe-flapping&ui=v2");
  await expect(
    pipelineHeader.getByRole("heading", { name: "Recovered Sink Flap" }),
  ).toBeVisible();
  await expect(inputStatus).toContainText("Live input");
  await expect(inputStatus).toContainText("30 audio tracks");
  await expect(inputStatus.getByText("Track 6")).toBeVisible();
  await expect(inputStatus.getByText("Track 7")).toHaveCount(0);
  await expect(
    inputStatus.getByRole("button", { name: "Show all 30" }),
  ).toBeVisible();
  await inputStatus.getByRole("button", { name: "Show all 30" }).click();
  await expect(inputStatus.getByText("Track 30")).toBeVisible();
  await expect(
    inputStatus.getByRole("button", { name: "Show fewer" }),
  ).toBeVisible();
  await expect(outputOverview).toContainText("SRT Sink Flap");
  await expect(outputOverview).toContainText("Flapping");
  await expect(outputOverview).toContainText("4 recent failures");

  await page.goto("/?mode=pipeline&view=operate&p=pipe-stall&ui=v2");
  await expect(
    pipelineHeader.getByRole("heading", { name: "Stalled Sink Isolation" }),
  ).toBeVisible();
  await expect(outputOverview).toContainText("RTMP stalled sink");
  await expect(outputOverview).toContainText("Stalled");
  await expect(outputOverview).toContainText("No progress for 10s");
  await expect(outputOverview).toContainText("5/6 active");
  const outputSearch = outputOverview.getByLabel("Search output destinations");
  await outputSearch.fill("stalled");
  await expect(outputOverview.getByText("1/6 shown")).toBeVisible();
  await expect(
    outputOverview.getByRole("heading", { name: "RTMP stalled sink" }),
  ).toBeVisible();
  await expect(
    outputOverview.getByRole("heading", { name: "Healthy sibling 01" }),
  ).not.toBeVisible();
  await outputSearch.fill("");
  await outputOverview.getByRole("button", { name: "Attention" }).click();
  await expect(
    outputOverview.getByRole("heading", { name: "RTMP stalled sink" }),
  ).toBeVisible();
  await expect(
    outputOverview.getByRole("heading", { name: "Healthy sibling 01" }),
  ).not.toBeVisible();

  await page.goto(
    "/?mode=pipeline&view=operate&p=pipe-retry-budget&ui=v2",
  );
  await expect(
    pipelineHeader.getByRole("heading", { name: "Retry Budget Exhausted" }),
  ).toBeVisible();
  await expect(outputOverview).toContainText("0/2 active");
  await expect(outputOverview.getByText("Error2")).toBeVisible();
  await expect(outputOverview).toContainText("RTMP dead sink");
  await expect(outputOverview).toContainText("Connection refused");
  await expect(outputOverview).toContainText("SRT dead sink");
  await expect(outputOverview).toContainText("connection failed");
  await expect(outputOverview).not.toContainText("2 Flapping");
  await expect(
    outputOverview.getByRole("heading", { name: "Needs attention" }),
  ).toBeVisible();
  await expect(
    outputOverview.getByRole("heading", { name: "RTMP dead sink" }),
  ).toBeVisible();
  await expect(
    outputOverview.getByRole("heading", { name: "SRT dead sink" }),
  ).toBeVisible();
});

test("seed: ui=v2 replaces Overview while delegating operator actions @desktop", async ({
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
  const seamErrors: string[] = [];
  page.on("pageerror", (error) => pageErrors.push(error.message));
  page.on("console", (message) => {
    if (
      message.type() === "error" &&
      message.text().includes("Unable to start the dashboard v2 experiment")
    ) {
      seamErrors.push(message.text());
    }
  });
  await openSeededDashboard(page, "mixed-health", "/?mode=overview&ui=v2", {
    outputControlDelayMs: 300,
    pipelineControlDelayMs: 300,
    settingsResponse: (settings) => ({
      ...settings,
      pipelines: (
        settings.pipelines as Array<Record<string, unknown>>
      ).map((pipeline) =>
        pipeline.id === "pipe-healthy"
          ? {
              ...pipeline,
              streamKey: "synthetic-healthy-stream-key-12345",
              ingestUrls: {
                rtmp:
                  "rtmp://ingest.example.invalid/live/synthetic-healthy-stream-key-12345",
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
  });

  await expect(page).toHaveURL(/\?mode=overview&ui=v2$/);
  await page.waitForTimeout(100);
  expect(pageErrors).toEqual([]);
  expect(seamErrors).toEqual([]);
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
    search: "?mode=overview&ui=v2",
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
      name: "Retrying Destination",
      exact: true,
    }),
  ).toBeVisible();
  await expect(
    v2Overview.getByRole("heading", { name: "Restream Activity" }),
  ).toBeVisible();
  await expect(page.locator("#overview-mode-content")).toBeHidden();

  await v2Overview
    .getByRole("button", { name: "Add Pipeline", exact: true })
    .click();
  await expect(page.locator("#edit-pipe-modal")).toBeVisible();
  await page.locator("#edit-pipe-modal").press("Escape");
  await v2Overview
    .getByRole("button", { name: "Operate", exact: true })
    .click();
  await expect(page).toHaveURL(/mode=pipeline.*ui=v2|ui=v2.*mode=pipeline/);
  await expect(page.locator("#dashboard-grid")).toBeVisible();
  const pipelineSelector = page.locator("#dashboard-v2-pipeline-selector-root");
  await expect(pipelineSelector).toBeVisible();
  await expect(
    pipelineSelector.getByRole("heading", { name: "Pipelines" }),
  ).toBeVisible();
  await expect(page.locator("#pipeline-selector-legacy")).toBeHidden();
  await expect(
    pipelineSelector.getByRole("button", {
      name: /Retrying Destination/,
    }),
  ).toHaveAttribute("aria-current", "page");
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
  const startFile = pipelineHeader.getByRole("button", { name: "Start File" });
  await startFile.click();
  await expect(
    pipelineHeader.getByRole("button", { name: "Starting File..." }),
  ).toBeDisabled();
  await expect(
    pipelineHeader.getByRole("button", { name: "Stop File" }),
  ).toBeEnabled();
  await pipelineHeader.getByRole("button", { name: "Stop File" }).click();
  await expect(
    pipelineHeader.getByRole("button", { name: "Stopping File..." }),
  ).toBeDisabled();
  await expect(startFile).toBeEnabled();
  await pipelineHeader.getByRole("button", { name: "Stop Rec" }).click();
  await expect(
    pipelineHeader.getByRole("button", { name: "Stopping..." }),
  ).toBeDisabled();
  await expect(
    pipelineHeader.getByRole("button", { name: "Record" }),
  ).toBeEnabled();
  await pipelineHeader.getByRole("button", { name: "Record" }).click();
  await expect(
    pipelineHeader.getByRole("button", { name: "Starting..." }),
  ).toBeDisabled();
  await expect(
    pipelineHeader.getByRole("button", { name: "Stop Rec" }),
  ).toBeEnabled();
  await expect(
    pipelineHeader.getByRole("button", { name: "Edit" }),
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
      .getByRole("button", { name: "More actions for Retrying Output" })
      .click();
  };
  await openRetryingOutputActions();
  await outputOverview
    .getByRole("button", { name: "History Retrying Output" })
    .click();
  await expect(page.locator("#output-history-modal")).toBeVisible();
  await expect(page.locator("#output-history-title")).toHaveText(
    "History: Retrying Output",
  );
  await page.locator("#output-history-modal").press("Escape");
  await openRetryingOutputActions();
  await outputOverview
    .getByRole("button", { name: "Monitor Retrying Output" })
    .click();
  expect(
    await page.evaluate(
      () =>
        (
          window as Window & { __redesignOpenedUrls?: string[] }
        ).__redesignOpenedUrls,
    ),
  ).toEqual(["https://monitor.example.invalid/retrying"]);
  await openRetryingOutputActions();
  await outputOverview
    .getByRole("button", { name: "Edit Retrying Output" })
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
    outputOverview.getByRole("button", { name: "Delete Retrying Output" }),
  ).toBeEnabled();
  await openRetryingOutputActions();
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
  await openRetryingOutputActions();
  await expect(
    outputOverview.getByRole("button", { name: "Delete Retrying Output" }),
  ).toBeEnabled();
  await outputOverview
    .getByRole("button", { name: "Delete Retrying Output" })
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
    .getByRole("button", { name: "Add Output", exact: true })
    .click();
  await expect(page.locator("#edit-out-modal")).toBeVisible();
  await expect(page.locator("#out-modal-title")).toHaveText(
    'Add Output for "Retrying Destination"',
  );
  await page.locator("#edit-out-modal").press("Escape");

  await pipelineSelector
    .getByRole("button", { name: /Healthy Program/ })
    .click();
  await expect(page).toHaveURL(/p=pipe-healthy/);
  await expect(page.locator("#pipe-name")).toHaveText("Healthy Program");
  await expect(pipelineHeader).toBeVisible();
  await expect(
    pipelineHeader.getByRole("heading", { name: "Healthy Program" }),
  ).toBeVisible();
  await expect(pipelineHeader).toContainText("RTMP");
  await expect(pipelineHeader).toContainText("Live");
  await expect(
    pipelineHeader.getByRole("button", { name: "Graph" }),
  ).toBeEnabled();
  await expect(
    pipelineHeader.getByRole("button", { name: "Diagnose" }),
  ).toBeEnabled();
  await expect(page.locator("#pipeline-header-legacy-identity")).toBeHidden();
  await expect(page.locator("#graph-pipe-btn")).toBeHidden();
  await expect(page.locator("#diagnose-pipe-btn")).toBeHidden();
  await expect(page.locator("#edit-pipe-action-item")).toBeHidden();
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
  await expect(
    page.locator("#pipeline-input-legacy-traffic"),
  ).toBeHidden();
  await expect(page.locator("#pipeline-input-legacy-video")).toBeHidden();
  await expect(page.locator("#video-player")).toBeHidden();
  const previewPlayer = inputStatus.locator(
    '[data-role="dashboard-v2-input-preview"]',
  );
  await expect(previewPlayer).toBeVisible();
  await expect(
    previewPlayer.getByRole("button", { name: "Play preview" }),
  ).toBeVisible();
  await expect(page.locator("#input-stats")).toBeHidden();
  await expect(
    page.locator("#pipeline-input-legacy-audio-heading"),
  ).toBeHidden();
  await expect(page.locator("#input-audio-tracks")).toBeHidden();
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
  await audioTrackName.fill("Discarded label");
  await audioTrackName.press("Escape");
  await expect(
    inputStatus.getByText("Program Audio", { exact: true }),
  ).toBeVisible();
  await expect(inputStatus.getByText("Discarded label")).toHaveCount(0);
  await expect(page.locator("#stream-key-section")).toBeHidden();
  await expect(page.locator("#ingest-url-section")).toBeHidden();
  await expect(inputStatus).toContainText("synthetic-healthy-st***12345");
  await inputStatus
    .getByRole("button", { name: "SRT", exact: true })
    .click();
  await expect(
    inputStatus.getByRole("button", { name: "SRT", exact: true }),
  ).toHaveAttribute("aria-pressed", "true");
  await expect(inputStatus).toContainText("srt://ingest.example.invalid:9000");
  await expect(
    inputStatus.getByRole("button", { name: "Copy SRT ingest URL" }),
  ).toBeVisible();
  await expect(outputOverview).toContainText("Running");
  await expect(outputOverview).toContainText("No outputs need attention");

  await pipelineHeader.getByRole("button", { name: "Edit" }).click();
  await expect(page.locator("#edit-pipe-modal")).toBeVisible();
  await expect(page.locator("#pipe-modal-title")).toHaveText("Edit Pipeline");
  await page.locator("#edit-pipe-modal").press("Escape");

  await pipelineSelector.getByRole("button", { name: "Add" }).click();
  await expect(page.locator("#edit-pipe-modal")).toBeVisible();
});

test("ui=v2 output cards keep 125-output refreshes patch-only @desktop", async ({
  page,
}) => {
  await page.goto("/login");
  await page.setContent(`
    <div id="dashboard-v2-root"></div>
    <div id="dashboard-v2-pipeline-selector-root"></div>
    <div id="dashboard-v2-pipeline-header-root"></div>
    <div id="dashboard-v2-pipeline-input-status-root"></div>
    <div id="dashboard-v2-pipeline-output-overview-root"></div>
  `);

  const result = await page.evaluate(async () => {
    const importModule = new Function(
      "path",
      "return import(path)",
    ) as (path: string) => Promise<{
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
    renderDashboardV2PipelineOutputOverview(model(cards, true), actions);
    await frame();
    const expandedCards = root.querySelectorAll("article").length;

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
      expandedCards,
      stable: await measure(false),
      live: await measure(true),
    };
  });

  console.log(`react-output-card-benchmark=${JSON.stringify(result)}`);
  expect(result.boundedCards).toBe(8);
  expect(result.expandedCards).toBe(125);
  expect(result.stable).toMatchObject({
    attributes: 0,
    characterData: 0,
    childList: 0,
  });
  expect(result.live.characterData).toBeGreaterThan(0);
  expect(result.live.childList).toBe(0);
});

test("ui=v2 pipeline details placeholder makes convergence explicit @desktop", async ({
  page,
}) => {
  await page.goto("/login");
  await page.setContent(`
    <div id="dashboard-v2-root"></div>
    <div id="dashboard-v2-pipeline-selector-root"></div>
    <div id="dashboard-v2-pipeline-header-root"></div>
    <div id="dashboard-v2-pipeline-input-status-root"></div>
    <div id="dashboard-v2-pipeline-output-overview-root"></div>
  `);

  await page.evaluate(async () => {
    const importModule = new Function(
      "path",
      "return import(path)",
    ) as (path: string) => Promise<{
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
    const importModule = new Function(
      "path",
      "return import(path)",
    ) as (path: string) => Promise<{
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

test("ui=v2 keeps failed recording mutation context in the pipeline header @desktop", async ({
  page,
}) => {
  await openSeededDashboard(
    page,
    "mixed-health",
    "/?mode=pipeline&view=operate&p=pipe-healthy&ui=v2",
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

  await header.getByRole("button", { name: "Record" }).click();
  await expect(
    header.getByRole("button", { name: "Starting..." }),
  ).toBeDisabled();
  await expect(
    header.getByRole("status").filter({
      hasText: "Recording request failed",
    }),
  ).toBeVisible();
  await expect(header).toContainText("Start recording did not complete");
  await expect(
    header.getByRole("button", { name: "Record" }),
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

test("ui=v2 keeps failed file-ingest mutation context in the pipeline header @desktop", async ({
  page,
}) => {
  await openSeededDashboard(
    page,
    "mixed-health",
    "/?mode=pipeline&view=operate&p=pipe-retrying&ui=v2",
    {
      expectOverviewReady: false,
      failFileIngestControl: "file source disappeared before ingest start",
      pipelineControlDelayMs: 250,
      settingsResponse: (settings) => ({
        ...settings,
        pipelines: (
          settings.pipelines as Array<Record<string, unknown>>
        ).map((pipeline) =>
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

  await header.getByRole("button", { name: "Start File" }).click();
  await expect(
    header.getByRole("button", { name: "Starting File..." }),
  ).toBeDisabled();
  await expect(
    header.getByRole("status").filter({
      hasText: "File ingest request failed",
    }),
  ).toBeVisible();
  await expect(header).toContainText("Start file ingest did not complete");
  await expect(
    header.getByRole("button", { name: "Start File" }),
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

test("ui=v2 keeps failed output mutation context on the output card @desktop", async ({
  page,
}) => {
  await openSeededDashboard(
    page,
    "mixed-health",
    "/?mode=pipeline&view=operate&p=pipe-healthy&ui=v2",
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

test("ui=v2 output destinations support search and state filters @desktop", async ({
  page,
}) => {
  await page.goto("/login");
  await page.setContent(`
    <div id="dashboard-v2-root"></div>
    <div id="dashboard-v2-pipeline-selector-root"></div>
    <div id="dashboard-v2-pipeline-header-root"></div>
    <div id="dashboard-v2-pipeline-input-status-root"></div>
    <div id="dashboard-v2-pipeline-output-overview-root"></div>
  `);

  await page.evaluate(async () => {
    const importModule = new Function(
      "path",
      "return import(path)",
    ) as (path: string) => Promise<{
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

  await root.getByRole("button", { name: "Stopped" }).click();
  await expect(root.getByText("No outputs match.")).toBeVisible();

  await root.getByRole("button", { name: "Clear filters" }).click();
  await expect(
    root.getByRole("heading", { name: "YouTube primary" }),
  ).toBeVisible();
  await expect(
    root.getByRole("heading", { name: "Facebook backup" }),
  ).toBeVisible();
  await expect(root.getByRole("heading", { name: "Archive" })).toBeVisible();

  await root.getByRole("button", { name: "Attention" }).click();
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
