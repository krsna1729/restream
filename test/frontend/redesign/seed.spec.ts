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
  await outputOverview
    .getByRole("button", { name: "History Retrying Output" })
    .click();
  await expect(page.locator("#output-history-modal")).toBeVisible();
  await expect(page.locator("#output-history-title")).toHaveText(
    "History: Retrying Output",
  );
  await page.locator("#output-history-modal").press("Escape");
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
  await expect(
    outputOverview.getByRole("button", { name: "Delete Retrying Output" }),
  ).toBeEnabled();
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
        for (const record of records) mutations[record.type] += 1;
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
