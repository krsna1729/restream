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

test("seed: ui=v2 Media search announces filtered result counts @desktop", async ({
  page,
}) => {
  await openSeededDashboard(page, "mixed-health", "/?mode=media&ui=v2", {
    expectOverviewReady: false,
  });

  const media = page.locator("#media-mode-panel");
  const checkpoint = media.locator("#dashboard-v2-media-root");
  const search = media.getByLabel("Search media library");
  const summary = media.locator("#media-library-results-summary");
  await expect(checkpoint.locator("#dashboard-v2-media-title")).toBeVisible();
  await expect(media.getByLabel("Search media library")).toBeVisible();
  await expect(summary).toHaveText(
    "1 media file total · 0 recordings · 1 source file",
  );
  await expect(
    checkpoint
      .getByText("1 media file total · 0 recordings · 1 source file")
      .first(),
  ).toBeVisible();
  await expect(
    checkpoint.getByText("1 source file", { exact: true }),
  ).toBeVisible();
  const sourceRow = media.locator('[data-filename="synthetic-source.mp4"]');
  await expect(
    sourceRow.getByRole("link", { name: "Download synthetic-source.mp4" }),
  ).toHaveCount(0);
  await expect(
    sourceRow.getByRole("button", {
      name: "Show media actions for synthetic-source.mp4",
    }),
  ).toHaveAttribute("aria-expanded", "false");
  const initialMediaButtonNames = await getCdpNamesByRole(page, "button");
  expect(initialMediaButtonNames).toContain("Upload media file");
  expect(initialMediaButtonNames).toContain(
    "Play unavailable for synthetic-source.mp4",
  );
  expect(initialMediaButtonNames).not.toContain("Upload media");
  expect(initialMediaButtonNames).not.toContain(
    "Show actions for synthetic-source.mp4",
  );
  expect(initialMediaButtonNames).not.toContain(
    "Play synthetic-source.mp4 unavailable",
  );
  await expect(sourceRow.getByRole("button", { name: "Rename" })).toHaveCount(
    0,
  );
  await expect(sourceRow.getByRole("button", { name: "Delete" })).toHaveCount(
    0,
  );
  await sourceRow
    .getByRole("button", {
      name: "Show media actions for synthetic-source.mp4",
    })
    .click();
  await expect(
    sourceRow.getByRole("button", {
      name: "Hide media actions for synthetic-source.mp4",
    }),
  ).toHaveAttribute("aria-expanded", "true");
  await expect(
    sourceRow.getByRole("link", { name: "Download synthetic-source.mp4" }),
  ).toBeVisible();
  await expect(
    sourceRow.getByRole("button", { name: "Rename synthetic-source.mp4" }),
  ).toBeVisible();
  await expect(
    sourceRow.getByRole("button", { name: "Delete synthetic-source.mp4" }),
  ).toBeVisible();
  const expandedMediaButtonNames = await getCdpNamesByRole(page, "button");
  const expandedMediaLinkNames = await getCdpNamesByRole(page, "link");
  expect(expandedMediaButtonNames).toEqual(
    expect.arrayContaining([
      "Rename synthetic-source.mp4",
      "Delete synthetic-source.mp4",
    ]),
  );
  expect(expandedMediaLinkNames).toContain("Download synthetic-source.mp4");
  expect(expandedMediaButtonNames).not.toContain("Rename");
  expect(expandedMediaButtonNames).not.toContain("Delete");
  expect(expandedMediaLinkNames).not.toContain("Download");
  await sourceRow
    .getByRole("button", {
      name: "Hide media actions for synthetic-source.mp4",
    })
    .click();
  await expect(
    sourceRow.getByRole("link", { name: "Download synthetic-source.mp4" }),
  ).toHaveCount(0);
  await expect(
    sourceRow.getByRole("button", { name: "Rename synthetic-source.mp4" }),
  ).toHaveCount(0);

  await search.fill("synthetic");
  await expect(summary).toHaveText(
    '1/1 media file shown · 0 recordings · 1 source file matched · "synthetic"',
  );
  await expect(
    checkpoint.getByText(
      '1/1 media file shown · 0 recordings · 1 source file matched · "synthetic"',
    ).first(),
  ).toBeVisible();
  await expect(media.getByText("synthetic-source.mp4")).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    '1/1 media file shown · 0 recordings · 1 source file matched · "synthetic"',
  );

  await search.fill("missing");
  await expect(summary).toHaveText(
    '0/1 media files shown · 0 recordings · 0 source files matched · "missing"',
  );
  await expect(
    checkpoint.getByText(
      '0/1 media files shown · 0 recordings · 0 source files matched · "missing"',
    ).first(),
  ).toBeVisible();
  await expect(
    media.getByText(
      'No recordings match "missing". Clear search to return to the full recording/source split.',
    ),
  ).toBeVisible();
  await expect(
    media.getByText(
      'No source files match "missing". Clear search to return to the full recording/source split.',
    ),
  ).toBeVisible();
  const clearSearch = media.getByRole("button", {
    name: "Clear media library search",
  });
  await expect(clearSearch).toBeVisible();
  const filteredMediaButtonNames = await getCdpNamesByRole(page, "button");
  expect(filteredMediaButtonNames).toContain("Clear media library search");
  expect(filteredMediaButtonNames).not.toContain("Clear search");
  expect(await getCdpStatusTexts(page)).toContain(
    '0/1 media files shown · 0 recordings · 0 source files matched · "missing"',
  );

  await clearSearch.click();
  await expect(search).toHaveValue("");
  await expect(summary).toHaveText(
    "1 media file total · 0 recordings · 1 source file",
  );
  await expect(
    checkpoint
      .getByText("1 media file total · 0 recordings · 1 source file")
      .first(),
  ).toBeVisible();
  await expect(clearSearch).toBeHidden();
  await expect(media.getByText("synthetic-source.mp4")).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    "1 media file total · 0 recordings · 1 source file",
  );
  expect(await getCdpNodeCount(page)).toBeLessThan(6_000);
});

test("seed: ui=v2 Media bounds dense libraries until requested @desktop", async ({
  page,
}) => {
  await openSeededDashboard(page, "mixed-health", "/?mode=media&ui=v2", {
    expectOverviewReady: false,
    mediaResponse: () => ({
      files: [
        ...Array.from({ length: 12 }, (_, index) => ({
          name: `dense-recording-${String(index + 1).padStart(2, "0")}.ts`,
          kind: "recording",
          size: 512_000 + index,
          modifiedAt: `2026-07-${String(20 - index).padStart(2, "0")}T00:00:00Z`,
          conversionStatus: index === 11 ? "ready" : "pending",
          convertedName: index === 11 ? "dense-recording-12.mp4" : undefined,
          playName: index === 11 ? "dense-recording-12.mp4" : undefined,
        })),
        ...Array.from({ length: 14 }, (_, index) => ({
          name: `dense-source-${String(index + 1).padStart(2, "0")}.mp4`,
          kind: "source",
          size: 1_024_000 + index,
          modifiedAt: `2026-06-${String(20 - index).padStart(2, "0")}T00:00:00Z`,
          ingestCount: index === 13 ? 2 : 0,
        })),
      ],
    }),
  });

  const media = page.locator("#media-mode-panel");
  const checkpoint = media.locator("#dashboard-v2-media-root");
  const summary = media.locator("#media-library-results-summary");
  await expect(summary).toHaveText(
    "26 media files total · 12 recordings · 14 source files",
  );
  await expect(
    checkpoint
      .getByText("26 media files total · 12 recordings · 14 source files")
      .first(),
  ).toBeVisible();
  await expect(
    checkpoint.getByText("12 recordings", { exact: true }),
  ).toBeVisible();
  await expect(
    checkpoint.getByText("14 source files", { exact: true }),
  ).toBeVisible();
  await expect(media.locator("#media-recordings-summary")).toHaveText(
    /8 shown of 12 files/,
  );
  await expect(media.locator("#media-sources-summary")).toHaveText(
    /8 shown of 14 files/,
  );
  await expect(media.getByText("dense-recording-01.ts")).toBeVisible();
  await expect(media.getByText("dense-recording-09.ts")).toHaveCount(0);
  await expect(media.getByText("dense-source-01.mp4")).toBeVisible();
  await expect(media.getByText("dense-source-09.mp4")).toHaveCount(0);
  const showAllRecordings = media.getByRole("button", {
    name: "Show all 12 recordings",
  });
  const showAllSources = media.getByRole("button", {
    name: "Show all 14 source files",
  });
  await expect(showAllRecordings).toHaveAttribute("aria-expanded", "false");
  await expect(showAllSources).toHaveAttribute("aria-expanded", "false");
  const denseMediaButtonNames = await getCdpNamesByRole(page, "button");
  expect(denseMediaButtonNames).toEqual(
    expect.arrayContaining([
      "Upload media file",
      "Show all 12 recordings",
      "Show all 14 source files",
    ]),
  );
  expect(denseMediaButtonNames).not.toContain("Upload media");
  expect(denseMediaButtonNames).not.toContain("Show all 12");
  expect(denseMediaButtonNames).not.toContain("Show all 14");
  expect(await getCdpNodeCount(page)).toBeLessThan(8_000);

  await showAllRecordings.click();
  await expect(media.locator("#media-recordings-toggle")).toHaveAttribute(
    "aria-expanded",
    "true",
  );
  await expect(media.getByText("dense-recording-12.ts")).toBeVisible();
  await showAllSources.click();
  await expect(media.locator("#media-sources-toggle")).toHaveAttribute(
    "aria-expanded",
    "true",
  );
  await expect(media.getByText("dense-source-14.mp4")).toBeVisible();

  const search = media.getByLabel("Search media library");
  await search.fill("dense-source-14");
  await expect(summary).toHaveText(
    '1/26 media file shown · 0 recordings · 1 source file matched · "dense-source-14"',
  );
  await expect(
    checkpoint.getByText(
      '1/26 media file shown · 0 recordings · 1 source file matched · "dense-source-14"',
    ).first(),
  ).toBeVisible();
  await expect(media.getByText("dense-source-14.mp4")).toBeVisible();
  await expect(
    media.getByRole("button", { name: /Show (all|fewer)/ }),
  ).toHaveCount(0);
  expect(await getCdpStatusTexts(page)).toContain(
    '1/26 media file shown · 0 recordings · 1 source file matched · "dense-source-14"',
  );
  expect(await getCdpNodeCount(page)).toBeLessThan(8_000);
});

test("seed: ui=v2 Status announces loaded build and activity summary @desktop", async ({
  page,
}) => {
  await openSeededDashboard(page, "mixed-health", "/?mode=status&ui=v2", {
    expectOverviewReady: false,
  });

  const status = page.locator("#status-mode-panel");
  const checkpoint = status.locator("#dashboard-v2-status-root");
  await expect(checkpoint.locator("#dashboard-v2-status-title")).toBeVisible();
  await expect(checkpoint).toContainText(
    "Status loaded for seeded · commit seeded · 1 process log · 1 notable activity",
  );
  await expect(
    checkpoint.getByText("seeded · seeded", { exact: true }),
  ).toBeVisible();
  await expect(
    checkpoint.getByText("1 process log", { exact: true }),
  ).toBeVisible();
  await expect(
    checkpoint.getByText("1 notable activity", { exact: true }),
  ).toBeVisible();
  await expect(
    status.getByRole("button", { name: "Show toolchain details" }),
  ).toBeVisible();
  await expect(status.locator("#status-build-section table")).toHaveCount(0);
  await expect(status.locator("#status-system-section table")).toHaveCount(0);
  await expect(status.locator("#status-toolchain-section table")).toHaveCount(
    0,
  );
  await status.getByRole("button", { name: "Show toolchain details" }).click();
  await expect(status.locator("#status-toolchain-section table")).toHaveCount(
    1,
  );
  await expect(status.locator("#status-toolchain-section")).toContainText(
    "Target",
  );
  await status.getByRole("button", { name: "Hide toolchain details" }).click();
  await expect(status.locator("#status-toolchain-section table")).toHaveCount(
    0,
  );
  await expect(
    status.getByRole("button", { name: "Download status report" }),
  ).toBeHidden();
  const exportActions = status.getByRole("button", {
    name: "Show status export actions",
  });
  await expect(exportActions).toBeVisible();
  await expect(exportActions).toHaveAttribute("aria-expanded", "false");
  await exportActions.click();
  await expect(
    status.getByRole("button", { name: "Download status report" }),
  ).toBeVisible();
  await expect(
    status.getByRole("button", { name: "Copy SBOM file" }),
  ).toBeVisible();
  const headingNames = await getCdpNamesByRole(page, "heading");
  expect(headingNames).toEqual(
    expect.arrayContaining(["Status", "Recent Activity", "Process Log"]),
  );
  expect(headingNames).not.toEqual(
    expect.arrayContaining([
      "Application Build",
      "System",
      "Toolchain",
      "Native Libraries",
      "SBOM",
      "Export actions",
    ]),
  );
  await expect(status.getByLabel("Jump to status section")).toBeVisible();
  expect(await getCdpNamesByRole(page, "link")).not.toEqual(
    expect.arrayContaining([
      "Build",
      "System",
      "Toolchain",
      "Libraries",
      "SBOM",
      "Activity",
      "Logs",
    ]),
  );
  expect(await getCdpNamesByRole(page, "link")).not.toEqual(
    expect.arrayContaining([
      "Jump to build status",
      "Jump to system status",
      "Jump to toolchain details",
      "Jump to native library details",
      "Jump to SBOM details",
      "Jump to recent activity",
      "Jump to process logs",
    ]),
  );
  const hideExportActions = status.getByRole("button", {
    name: "Hide status export actions",
  });
  await expect(hideExportActions).toHaveAttribute("aria-expanded", "true");
  const statusButtonNames = await getCdpNamesByRole(page, "button");
  expect(statusButtonNames).toEqual(
    expect.arrayContaining([
      "Refresh status data",
      "Show application build details",
      "Show system details",
      "Show toolchain details",
      "Show native library details",
      "Show SBOM details",
      "Hide status export actions",
      "Download status report",
      "Copy SBOM file",
    ]),
  );
  expect(statusButtonNames).not.toEqual(
    expect.arrayContaining(["Download Status", "Copy SBOM"]),
  );
  expect(statusButtonNames).not.toEqual(
    expect.arrayContaining([
      "Show Toolchain details",
      "Show Native Libraries details",
    ]),
  );
  expect(statusButtonNames).not.toContain("Refresh");
  expect(statusButtonNames).not.toContain("Hide export actions");
  await hideExportActions.click();
  await expect(
    status.getByRole("button", { name: "Download status report" }),
  ).toBeHidden();
  expect(await getCdpStatusTexts(page)).toContain(
    "Status loaded for seeded · commit seeded · 1 process log · 1 notable activity",
  );
  const search = status.getByLabel("Search process logs and activity");
  expect(await getCdpNamesByRole(page, "searchbox")).toContain(
    "Search process logs and activity",
  );
  const searchSummary = status.locator("#status-log-search-results-summary");
  await expect(searchSummary).toHaveText("1 activity · 1 process log visible");

  await search.fill("synthetic");
  await expect(checkpoint).toContainText(
    "Status loaded for seeded · commit seeded · 1 process log · 1 notable activity",
  );
  await expect(searchSummary).toHaveText(
    '1 activity · 1 process log match "synthetic"',
  );
  await expect(
    checkpoint.getByText('1 activity · 1 process log match "synthetic"').first(),
  ).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    '1 activity · 1 process log match "synthetic"',
  );

  await search.fill("missing");
  await expect(checkpoint).toContainText(
    "Status loaded for seeded · commit seeded · 1 process log · 1 notable activity",
  );
  await expect(searchSummary).toHaveText(
    '0 activities · 0 process logs match "missing"',
  );
  await expect(
    checkpoint
      .getByText('0 activities · 0 process logs match "missing"')
      .first(),
  ).toBeVisible();
  await expect(
    status.getByText(
      'No activity entries match "missing". Clear search to return to the full status view.',
    ),
  ).toBeVisible();
  await expect(
    status.getByText(
      'No process log entries match "missing". Clear search to return to the full status view.',
    ),
  ).toBeVisible();
  const clearSearch = status.getByRole("button", {
    name: "Clear status search",
  });
  await expect(clearSearch).toBeVisible();
  const filteredStatusButtonNames = await getCdpNamesByRole(page, "button");
  expect(filteredStatusButtonNames).toContain("Clear status search");
  expect(filteredStatusButtonNames).not.toContain("Clear search");
  expect(await getCdpStatusTexts(page)).toContain(
    '0 activities · 0 process logs match "missing"',
  );

  await clearSearch.click();
  await expect(search).toHaveValue("");
  await expect(searchSummary).toHaveText("1 activity · 1 process log visible");
  await expect(
    checkpoint.getByText("1 activity · 1 process log visible").first(),
  ).toBeVisible();
  await expect(clearSearch).toBeHidden();
  await expect(
    status.getByText("Synthetic output entered retry backoff"),
  ).toHaveCount(2);
  expect(await getCdpStatusTexts(page)).toContain(
    "1 activity · 1 process log visible",
  );
  expect(await getCdpNodeCount(page)).toBeLessThan(8_000);
  await status
    .getByRole("button", { name: "Show application build details" })
    .click();
  await expect(status.locator("#status-build-section table")).toHaveCount(1);
  await status
    .getByRole("button", { name: "Hide application build details" })
    .click();
  await expect(status.locator("#status-build-section table")).toHaveCount(0);
  const statusSectionJump = status.getByLabel("Jump to status section");
  await statusSectionJump.selectOption("status-native-section");
  await expect(
    status.getByRole("button", { name: "Hide native library details" }),
  ).toBeVisible();
  await expect(page).toHaveURL(/#status-native-section$/);
});

test("seed: ui=v2 Status bounds dense process logs until requested @desktop", async ({
  page,
}) => {
  await openSeededDashboard(page, "mixed-health", "/?mode=status&ui=v2", {
    expectOverviewReady: false,
    logsResponse: () => ({
      logs: Array.from({ length: 35 }, (_, index) => ({
        id: 500 - index,
        ts: `2026-07-14T06:${String(index).padStart(2, "0")}:00Z`,
        level: index === 0 ? "INFO" : index % 7 === 0 ? "WARN" : "DEBUG",
        target: index === 0 ? "restream::server" : "restream::worker",
        message:
          index === 0
            ? "dashboard api server listening"
            : `routine status log ${index + 1}`,
        fields: "{}",
        pipelineId: null,
        outputId: null,
        eventType: index === 0 ? "restream.http.ready" : null,
      })),
    }),
  });

  const status = page.locator("#status-mode-panel");
  const checkpoint = status.locator("#dashboard-v2-status-root");
  await expect(checkpoint).toContainText(
    "Status loaded for seeded · commit seeded · 35 process logs · 5 notable activities",
  );
  await expect(
    checkpoint.getByText("35 process logs", { exact: true }),
  ).toBeVisible();
  await expect(
    checkpoint.getByText("5 notable activities", { exact: true }),
  ).toBeVisible();
  await expect(status.locator("#status-log-search-results-summary")).toHaveText(
    "5 activities · 35 process logs visible",
  );
  await expect(
    checkpoint.getByText("5 activities · 35 process logs visible").first(),
  ).toBeVisible();
  const logs = status.getByLabel("Process log entries");
  await expect(status.getByText("20 process logs shown of 35")).toBeVisible();
  await expect(logs.getByText("routine status log 20")).toBeVisible();
  await expect(logs.getByText("routine status log 21")).toHaveCount(0);
  const showAll = status.getByRole("button", {
    name: "Show all 35 process logs",
  });
  await expect(showAll).toHaveAttribute("aria-expanded", "false");
  const denseStatusButtonNames = await getCdpNamesByRole(page, "button");
  expect(denseStatusButtonNames).toContain("Show all 35 process logs");
  expect(denseStatusButtonNames).not.toContain("Show all 35");
  expect(await getCdpNodeCount(page)).toBeLessThan(10_000);

  await showAll.click();
  await expect(status.locator("#status-log-toggle")).toHaveAttribute(
    "aria-expanded",
    "true",
  );
  await expect(logs.getByText("routine status log 35")).toBeVisible();

  const search = status.getByLabel("Search process logs and activity");
  await search.fill("log 35");
  await expect(status.locator("#status-log-search-results-summary")).toHaveText(
    '0 activities · 1 process log match "log 35"',
  );
  await expect(
    checkpoint.getByText('0 activities · 1 process log match "log 35"').first(),
  ).toBeVisible();
  await expect(logs.getByText("routine status log 35")).toBeVisible();
  await expect(
    status.getByRole("button", { name: /Show (all|fewer)/ }),
  ).toHaveCount(0);
  expect(await getCdpStatusTexts(page)).toContain(
    '0 activities · 1 process log match "log 35"',
  );
  expect(await getCdpNodeCount(page)).toBeLessThan(10_000);
});

test("seed: ui=v2 Incidents announces scoped alert and event counts @desktop", async ({
  page,
}) => {
  await openSeededDashboard(page, "mixed-health", "/?mode=incidents&ui=v2", {
    expectOverviewReady: false,
  });

  const incidents = page.locator("#incidents-mode-panel");
  const checkpoint = incidents.locator("#dashboard-v2-incidents-root");
  await expect(
    checkpoint.locator("#dashboard-v2-incidents-title"),
  ).toBeVisible();
  await expect(checkpoint).toContainText(
    "0 critical · 1 warning · 1 recent event · fleet",
  );
  await expect(
    checkpoint.getByText("0 critical · 1 warning", { exact: true }),
  ).toBeVisible();
  await expect(
    checkpoint.getByText("1 recent event", { exact: true }),
  ).toBeVisible();
  await expect(
    checkpoint.getByText("1 alert group · 1 event visible").first(),
  ).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    "0 critical · 1 warning · 1 recent event · fleet",
  );
  const search = incidents.getByLabel("Search incidents and events");
  expect(await getCdpNamesByRole(page, "searchbox")).toContain(
    "Search incidents and events",
  );
  const searchSummary = incidents.locator("#incidents-search-results-summary");
  await expect(searchSummary).toHaveText("1 alert group · 1 event visible");
  const retryingAlert = incidents
    .locator("[data-alert-id='seed-alert-retrying-output']")
    .first();
  await expect(
    retryingAlert.getByRole("heading", { name: "Retrying output" }),
  ).toBeVisible();
  await expect(retryingAlert.getByText("Recommended action:")).toHaveCount(0);
  await expect(retryingAlert.getByText("Evidence")).toHaveCount(0);
  await expect(
    retryingAlert.getByRole("button", {
      name: "Show alert details for Retrying output",
    }),
  ).toHaveAttribute("aria-expanded", "false");
  await expect(
    retryingAlert.getByRole("button", {
      name: "Open pipeline Retrying Destination",
    }),
  ).toBeVisible();
  const initialIncidentButtonNames = await getCdpNamesByRole(page, "button");
  expect(initialIncidentButtonNames).toEqual(
    expect.arrayContaining([
      "Refresh incident data",
      "Show alert details for Retrying output",
      "Open pipeline Retrying Destination",
    ]),
  );
  expect(initialIncidentButtonNames).not.toContain("Refresh");
  expect(initialIncidentButtonNames).not.toContain("Show alert details");
  expect(initialIncidentButtonNames).not.toContain(
    "Open pipeline pipe-retrying",
  );
  await retryingAlert
    .getByRole("button", { name: "Show alert details for Retrying output" })
    .click();
  await expect(
    retryingAlert.getByRole("button", {
      name: "Hide alert details for Retrying output",
    }),
  ).toHaveAttribute("aria-expanded", "true");
  await expect(retryingAlert.getByText("Recommended action:")).toBeVisible();
  await expect(retryingAlert.getByText("Evidence")).toBeVisible();
  expect(await getCdpNamesByRole(page, "button")).toContain(
    "Hide alert details for Retrying output",
  );
  await retryingAlert
    .getByRole("button", { name: "Hide alert details for Retrying output" })
    .click();
  await expect(retryingAlert.getByText("Recommended action:")).toHaveCount(0);

  await search.fill("destination");
  await expect(search).toHaveValue("destination");
  await expect(checkpoint).toContainText(
    "0 critical · 1 warning · 1 recent event · fleet",
  );
  await expect(searchSummary).toHaveText(
    '1 alert group · 1 event match "destination"',
  );
  await expect(
    checkpoint
      .getByText('1 alert group · 1 event match "destination"')
      .first(),
  ).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    '1 alert group · 1 event match "destination"',
  );

  await search.fill("healthy");
  await expect(search).toHaveValue("healthy");
  await expect(checkpoint).toContainText(
    "0 critical · 1 warning · 1 recent event · fleet",
  );
  await expect(searchSummary).toHaveText(
    '0 alert groups · 0 events match "healthy"',
  );
  await expect(
    checkpoint.getByText('0 alert groups · 0 events match "healthy"').first(),
  ).toBeVisible();
  await expect(
    incidents.getByText(
      'No alerts match "healthy". Clear search to return to the full incident feed.',
    ),
  ).toBeVisible();
  await expect(
    incidents.getByText(
      'No events match "healthy". Clear search to return to the full incident feed.',
    ),
  ).toBeVisible();
  const clearSearch = incidents.getByRole("button", {
    name: "Clear incident search",
  });
  await expect(clearSearch).toBeVisible();
  const filteredIncidentButtonNames = await getCdpNamesByRole(page, "button");
  expect(filteredIncidentButtonNames).toContain("Clear incident search");
  expect(filteredIncidentButtonNames).not.toContain("Clear search");
  expect(await getCdpStatusTexts(page)).toContain(
    '0 alert groups · 0 events match "healthy"',
  );

  await clearSearch.click();
  await expect(search).toHaveValue("");
  await expect(searchSummary).toHaveText("1 alert group · 1 event visible");
  await expect(
    checkpoint.getByText("1 alert group · 1 event visible").first(),
  ).toBeVisible();
  await expect(clearSearch).toBeHidden();
  await expect(
    incidents.getByRole("heading", { name: "Retrying output" }),
  ).toBeVisible();
  await expect(
    incidents.getByText(
      'No alerts match "healthy". Clear search to return to the full incident feed.',
    ),
  ).toHaveCount(0);
  await expect(
    incidents.getByText(
      'No events match "healthy". Clear search to return to the full incident feed.',
    ),
  ).toHaveCount(0);
  expect(await getCdpStatusTexts(page)).toContain(
    "1 alert group · 1 event visible",
  );

  await incidents
    .getByLabel("Filter incidents by pipeline")
    .selectOption("pipe-healthy");
  await expect(checkpoint).toContainText(
    "0 critical · 0 warning · 0 recent events · Healthy Program",
  );
  await expect(
    checkpoint.getByText("0 critical · 0 warning", { exact: true }),
  ).toBeVisible();
  await expect(
    checkpoint.getByText("Healthy Program", { exact: true }).first(),
  ).toBeVisible();
  await expect(
    incidents.getByText("No active alerts for this pipeline."),
  ).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    "0 critical · 0 warning · 0 recent events · Healthy Program",
  );
  expect(await getCdpNodeCount(page)).toBeLessThan(8_000);
});

test("seed: ui=v2 Incidents bounds dense alert and event lists until requested @desktop", async ({
  page,
}) => {
  await openSeededDashboard(page, "mixed-health", "/?mode=incidents&ui=v2", {
    expectOverviewReady: false,
    alertsResponse: () => ({
      generatedAt: "2026-07-14T06:30:00Z",
      alerts: Array.from({ length: 14 }, (_, index) => ({
        id: `dense-alert-${index + 1}`,
        severity: index % 5 === 0 ? "critical" : "warning",
        scope: "output",
        pipelineId: "pipe-retrying",
        outputId: `out-dense-${String(index + 1).padStart(2, "0")}`,
        title: `Dense alert ${index + 1}`,
        cause: `Dense destination ${index + 1} entered retry backoff`,
        evidence: [`dense ${index + 1} lifecycle sample`],
        recommendedAction: "Inspect the destination endpoint.",
        generatedAt: "2026-07-14T06:29:54Z",
        firstSeen: "2026-07-14T06:29:54Z",
        lastSeen: "2026-07-14T06:29:54Z",
      })),
    }),
    eventsResponse: () => ({
      generatedAt: "2026-07-14T06:30:00Z",
      count: 20,
      events: Array.from({ length: 20 }, (_, index) => ({
        seq: 200 - index,
        timestamp: "2026-07-14T06:29:54Z",
        kind: `dense.event.${index + 1}`,
        pipelineId: "pipe-retrying",
        outputId: `out-dense-${String(index + 1).padStart(2, "0")}`,
        error: `dense ${index + 1} lifecycle sample`,
      })),
    }),
  });

  const incidents = page.locator("#incidents-mode-panel");
  const checkpoint = incidents.locator("#dashboard-v2-incidents-root");
  const summary = incidents.locator("#incidents-route-summary");
  await expect(summary).toHaveText(
    "3 critical · 11 warning · 20 recent events · fleet",
  );
  await expect(
    checkpoint.getByText("3 critical", { exact: true }),
  ).toBeVisible();
  await expect(
    checkpoint.getByText("14 alert groups · 20 events visible").first(),
  ).toBeVisible();
  await expect(
    incidents.locator("#incidents-search-results-summary"),
  ).toHaveText("14 alert groups · 20 events visible");
  await expect(incidents.getByText("8 alert groups shown of 14")).toBeVisible();
  await expect(
    incidents.getByRole("heading", { name: "Dense alert 14" }),
  ).toBeVisible();
  await expect(
    incidents.getByRole("heading", { name: "Dense alert 3" }),
  ).toHaveCount(0);
  const eventList = incidents.getByLabel("Incident lifecycle events");
  await expect(eventList.getByText("12 events shown of 20")).toBeVisible();
  await expect(eventList.getByText("dense.event.12")).toBeVisible();
  await expect(eventList.getByText("dense.event.13")).toHaveCount(0);
  const showAllAlerts = incidents.getByRole("button", {
    name: "Show all 14 incident alert groups",
  });
  const showAllEvents = eventList.getByRole("button", {
    name: "Show all 20 incident lifecycle events",
  });
  await expect(showAllAlerts).toHaveAttribute("aria-expanded", "false");
  await expect(showAllEvents).toHaveAttribute("aria-expanded", "false");
  await expect(
    incidents
      .getByRole("button", { name: "Open pipeline Retrying Destination" })
      .first(),
  ).toBeVisible();
  const denseIncidentButtonNames = await getCdpNamesByRole(page, "button");
  expect(denseIncidentButtonNames).toEqual(
    expect.arrayContaining([
      "Refresh incident data",
      "Show all 14 incident alert groups",
      "Show all 20 incident lifecycle events",
      "Show alert details for Dense alert 14",
      "Open pipeline Retrying Destination",
    ]),
  );
  expect(denseIncidentButtonNames).not.toContain("Refresh");
  expect(denseIncidentButtonNames).not.toContain("Show all 14");
  expect(denseIncidentButtonNames).not.toContain("Show all 20");
  expect(denseIncidentButtonNames).not.toContain("Show alert details");
  expect(denseIncidentButtonNames).not.toContain("Open pipeline pipe-retrying");
  expect(await getCdpNodeCount(page)).toBeLessThan(11_000);

  await showAllAlerts.click();
  await expect(incidents.locator("#incidents-alerts-toggle")).toHaveAttribute(
    "aria-expanded",
    "true",
  );
  await expect(
    incidents.getByRole("heading", { name: "Dense alert 14" }),
  ).toBeVisible();
  await showAllEvents.click();
  await expect(eventList.locator("#incidents-events-toggle")).toHaveAttribute(
    "aria-expanded",
    "true",
  );
  await expect(eventList.getByText("dense.event.20")).toBeVisible();

  const search = incidents.getByLabel("Search incidents and events");
  await search.fill("dense 14");
  await expect(
    incidents.locator("#incidents-search-results-summary"),
  ).toHaveText('1 alert group · 1 event match "dense 14"');
  await expect(
    incidents.getByRole("heading", { name: "Dense alert 14" }),
  ).toBeVisible();
  await expect(eventList.getByText("dense.event.14")).toBeVisible();
  await expect(
    incidents.getByRole("button", { name: /Show (all|fewer)/ }),
  ).toHaveCount(0);
  expect(await getCdpStatusTexts(page)).toContain(
    '1 alert group · 1 event match "dense 14"',
  );
  expect(await getCdpNodeCount(page)).toBeLessThan(11_000);
});

test("seed: ui=v2 Telemetry announces scoped engine and pipeline counts @desktop", async ({
  page,
}) => {
  await openSeededDashboard(page, "mixed-health", "/?mode=telemetry&ui=v2", {
    expectOverviewReady: false,
  });

  const telemetry = page.locator("#telemetry-mode-panel");
  const checkpoint = telemetry.locator("#dashboard-v2-telemetry-root");
  await expect(
    checkpoint.locator("#dashboard-v2-telemetry-title"),
  ).toBeVisible();
  await expect(checkpoint).toContainText(
    "Telemetry loaded · 2 ingests · 2 stages · 1 egress · 1 reader · Healthy Program",
  );
  await expect(
    checkpoint.getByText("Healthy Program", { exact: true }).first(),
  ).toBeVisible();
  await expect(
    checkpoint.getByText("2 stage counters", { exact: true }),
  ).toBeVisible();
  await expect(checkpoint.getByText("1 egress", { exact: true })).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    "Telemetry loaded · 2 ingests · 2 stages · 1 egress · 1 reader · Healthy Program",
  );
  const hostSettings = telemetry.getByLabel("Host settings");
  await expect(
    hostSettings.getByText("1 host setting · health ready"),
  ).toBeVisible();
  await expect(
    hostSettings.getByRole("button", { name: "Show telemetry host settings" }),
  ).toHaveAttribute("aria-expanded", "false");
  const initialTelemetryButtonNames = await getCdpNamesByRole(page, "button");
  expect(initialTelemetryButtonNames).toContain("Refresh telemetry data");
  expect(initialTelemetryButtonNames).not.toContain("Refresh");
  expect(await getCdpNamesByRole(page, "combobox")).toContain(
    "Filter telemetry by pipeline",
  );
  expect(await getCdpNamesByRole(page, "combobox")).not.toContain(
    "Telemetry pipeline",
  );
  await expect(hostSettings.getByText("Open file descriptors")).toHaveCount(0);
  await hostSettings
    .getByRole("button", { name: "Show telemetry host settings" })
    .click();
  await expect(
    hostSettings.getByRole("button", { name: "Hide telemetry host settings" }),
  ).toHaveAttribute("aria-expanded", "true");
  await expect(hostSettings.getByText("Open file descriptors")).toBeVisible();
  await hostSettings
    .getByRole("button", { name: "Hide telemetry host settings" })
    .click();
  await expect(hostSettings.getByText("Open file descriptors")).toHaveCount(0);

  await telemetry
    .getByLabel("Filter telemetry by pipeline")
    .selectOption("pipe-retrying");
  await expect(checkpoint).toContainText(
    "Telemetry loaded · 2 ingests · 2 stages · 1 egress · 1 reader · Retrying Destination",
  );
  await expect(
    checkpoint.getByText("Retrying Destination", { exact: true }).first(),
  ).toBeVisible();
  const search = telemetry.getByLabel("Search telemetry items");
  expect(await getCdpNamesByRole(page, "searchbox")).toContain(
    "Search telemetry items",
  );
  const searchSummary = telemetry.locator("#telemetry-search-results-summary");
  await expect(searchSummary).toHaveText(
    "1 reader · 2 stages · 1 egress visible",
  );

  await search.fill("video");
  await expect(searchSummary).toHaveText(
    '1/4 telemetry items match "video" · 0 readers · 1 stage · 0 egresses',
  );
  await expect(
    checkpoint.getByText(
      '1/4 telemetry items match "video" · 0 readers · 1 stage · 0 egresses',
    ).first(),
  ).toBeVisible();
  await expect(
    telemetry.getByText(
      'No readers match "video". Clear search to return to the full telemetry set.',
    ),
  ).toBeVisible();
  await expect(
    telemetry.getByText(
      'No egresses match "video". Clear search to return to the full telemetry set.',
    ),
  ).toBeVisible();
  await expect(
    telemetry.getByRole("button", { name: "View video telemetry details" }),
  ).toBeVisible();
  const headingNames = await getCdpNamesByRole(page, "heading");
  expect(headingNames).toEqual(
    expect.arrayContaining([
      "Engineer telemetry",
      "Processing stages",
      "Stage detail",
    ]),
  );
  expect(headingNames).not.toEqual(expect.arrayContaining(["video", "audio"]));
  expect(await getCdpStatusTexts(page)).toContain(
    '1/4 telemetry items match "video" · 0 readers · 1 stage · 0 egresses',
  );

  await search.fill("absent");
  await expect(searchSummary).toHaveText(
    '0/4 telemetry items match "absent" · 0 readers · 0 stages · 0 egresses',
  );
  await expect(
    checkpoint.getByText(
      '0/4 telemetry items match "absent" · 0 readers · 0 stages · 0 egresses',
    ).first(),
  ).toBeVisible();
  const clearSearch = telemetry.getByRole("button", {
    name: "Clear telemetry search",
  });
  await expect(clearSearch).toBeVisible();
  const filteredTelemetryButtonNames = await getCdpNamesByRole(page, "button");
  expect(filteredTelemetryButtonNames).toContain("Clear telemetry search");
  expect(filteredTelemetryButtonNames).not.toContain("Clear search");
  await expect(
    telemetry.getByText(
      'No stages match "absent". Clear search to return to the full telemetry set.',
    ),
  ).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    '0/4 telemetry items match "absent" · 0 readers · 0 stages · 0 egresses',
  );

  await clearSearch.click();
  await expect(search).toHaveValue("");
  await expect(searchSummary).toHaveText(
    "1 reader · 2 stages · 1 egress visible",
  );
  await expect(
    checkpoint.getByText("1 reader · 2 stages · 1 egress visible").first(),
  ).toBeVisible();
  await expect(clearSearch).toBeHidden();
  await expect(telemetry.getByText("retrying-output-reader")).toBeVisible();
  await expect(
    telemetry.getByText("1 counter · raw values in Stage detail").first(),
  ).toBeVisible();
  await expect(telemetry.locator("#stage-telemetry-detail")).not.toContainText(
    "packetsOut",
  );
  await telemetry
    .getByRole("button", { name: "View video telemetry details" })
    .click();
  await expect(telemetry.locator("#stage-telemetry-detail")).toContainText(
    "packetsOut",
  );
  await expect(
    telemetry.getByRole("button", {
      name: "Hide stage details for pipe-retrying:video",
    }),
  ).toBeVisible();
  expect(await getCdpNamesByRole(page, "button")).toContain(
    "Hide stage details for pipe-retrying:video",
  );
  await telemetry
    .getByRole("button", {
      name: "Hide stage details for pipe-retrying:video",
    })
    .click();
  await expect(telemetry.locator("#stage-telemetry-detail")).not.toContainText(
    "packetsOut",
  );
  await expect(
    telemetry
      .locator("#stage-telemetry-detail")
      .getByText("Select a stage to fetch its current detail."),
  ).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    "Telemetry loaded · 2 ingests · 2 stages · 1 egress · 1 reader · Retrying Destination",
  );
  expect(await getCdpNodeCount(page)).toBeLessThan(8_000);
});

test("seed: ui=v2 Telemetry bounds dense stage and egress lists until requested @desktop", async ({
  page,
}) => {
  await openSeededDashboard(page, "mixed-health", "/?mode=telemetry&ui=v2", {
    expectOverviewReady: false,
    pipelineTelemetryResponse: (pipelineId, telemetry) => {
      if (pipelineId !== "pipe-retrying") return telemetry;
      return {
        ...telemetry,
        stages: Array.from({ length: 12 }, (_, index) => ({
          pipelineId,
          stageKey: `${pipelineId}:dense-stage-${String(index + 1).padStart(2, "0")}`,
          kind: `dense-stage-${String(index + 1).padStart(2, "0")}`,
          active: true,
          metrics: { packetsOut: 1_000 + index },
        })),
        egresses: Array.from({ length: 12 }, (_, index) => ({
          pipelineId,
          outputId: `out-dense-${String(index + 1).padStart(2, "0")}`,
          status: "running",
          bytesOut: 65_536 + index,
        })),
      };
    },
  });

  const telemetry = page.locator("#telemetry-mode-panel");
  const checkpoint = telemetry.locator("#dashboard-v2-telemetry-root");
  await telemetry
    .getByLabel("Filter telemetry by pipeline")
    .selectOption("pipe-retrying");
  await expect(telemetry.locator("#dashboard-v2-telemetry-root")).toContainText(
    "Telemetry loaded · 2 ingests · 12 stages · 12 egresses · 1 reader · Retrying Destination",
  );
  await expect(
    checkpoint.getByText("12 egresses", { exact: true }),
  ).toBeVisible();
  await expect(
    checkpoint
      .getByText("1 reader · 12 stages · 12 egresses visible")
      .first(),
  ).toBeVisible();
  await expect(
    telemetry.locator("#telemetry-search-results-summary"),
  ).toHaveText("1 reader · 12 stages · 12 egresses visible");

  const stages = telemetry.getByLabel("Telemetry processing stages");
  await expect(stages.getByText("8 stages shown of 12")).toBeVisible();
  await expect(stages.getByText("dense-stage-08")).toBeVisible();
  await expect(stages.getByText("dense-stage-09")).toHaveCount(0);
  const showAllStages = stages.getByRole("button", {
    name: "Show all 12 telemetry stages",
  });
  await expect(showAllStages).toHaveAttribute("aria-expanded", "false");
  const egresses = telemetry.getByLabel("Telemetry egresses");
  await expect(egresses.getByText("8 egresses shown of 12")).toBeVisible();
  await expect(egresses.getByText("out-dense-08")).toBeVisible();
  await expect(egresses.getByText("out-dense-09")).toHaveCount(0);
  const showAll = egresses.getByRole("button", {
    name: "Show all 12 telemetry egresses",
  });
  await expect(showAll).toHaveAttribute("aria-expanded", "false");
  const denseTelemetryButtonNames = await getCdpNamesByRole(page, "button");
  expect(denseTelemetryButtonNames).toEqual(
    expect.arrayContaining([
      "Show all 12 telemetry stages",
      "Show all 12 telemetry egresses",
    ]),
  );
  expect(denseTelemetryButtonNames).not.toContain("Show all 12");
  expect(await getCdpNodeCount(page)).toBeLessThan(8_500);

  await showAllStages.click();
  await expect(stages.locator("#telemetry-stages-toggle")).toHaveAttribute(
    "aria-expanded",
    "true",
  );
  await expect(stages.getByText("dense-stage-12")).toBeVisible();
  await showAll.click();
  await expect(egresses.locator("#telemetry-egress-toggle")).toHaveAttribute(
    "aria-expanded",
    "true",
  );
  await expect(egresses.getByText("out-dense-12")).toBeVisible();

  const search = telemetry.getByLabel("Search telemetry items");
  await search.fill("out-dense-12");
  await expect(
    telemetry.locator("#telemetry-search-results-summary"),
  ).toHaveText(
    '1/25 telemetry items match "out-dense-12" · 0 readers · 0 stages · 1 egress',
  );
  await expect(
    checkpoint.getByText(
      '1/25 telemetry items match "out-dense-12" · 0 readers · 0 stages · 1 egress',
    ).first(),
  ).toBeVisible();
  await expect(
    stages.getByRole("button", { name: /Show (all|fewer)/ }),
  ).toHaveCount(0);
  await expect(egresses.getByText("out-dense-12")).toBeVisible();
  await expect(
    egresses.getByRole("button", { name: /Show (all|fewer)/ }),
  ).toHaveCount(0);
  expect(await getCdpStatusTexts(page)).toContain(
    '1/25 telemetry items match "out-dense-12" · 0 readers · 0 stages · 1 egress',
  );
  expect(await getCdpNodeCount(page)).toBeLessThan(8_500);
});

test("seed: ui=v2 Operate stays inside the viewport across breakpoints", async ({
  page,
}) => {
  await openSeededDashboard(
    page,
    "chaos-recovery",
    "/?mode=pipeline&view=operate&p=pipe-flapping&ui=v2",
    { expectOverviewReady: false },
  );

  await expect(
    page.locator("#dashboard-v2-pipeline-selector-root").getByText("Pipelines"),
  ).toBeVisible();
  await expect(
    page.locator("#dashboard-v2-pipeline-header-root").getByRole("heading", {
      name: "Recovered Sink Flap",
    }),
  ).toBeVisible();
  await expect(
    page
      .locator("#dashboard-v2-pipeline-input-status-root")
      .getByRole("heading", { name: "Input and preview" }),
  ).toBeVisible();
  await expect(
    page
      .locator("#dashboard-v2-pipeline-output-overview-root")
      .getByRole("heading", { name: "Output overview" }),
  ).toBeVisible();

  const pageOverflow = await getDocumentWidthOverflow(page);
  expect(pageOverflow).toBeLessThanOrEqual(1);
  expect(await getCdpLayoutWidthDelta(page)).toBeLessThanOrEqual(1);
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

  await expect(pipelineSelector.getByText("Pipelines")).toBeVisible();
  await expect(
    pipelineHeader.getByRole("heading", { name: "Transient Publisher Drop" }),
  ).toBeVisible();
  await expect(inputStatus).toContainText("Reconnecting");
  await expect(inputStatus).toContainText("Disconnect grace active");
  await expect(outputOverview).toContainText("Grace-preserved Output");
  await expect(outputOverview).toContainText("Running");

  await page.goto("/?mode=pipeline&view=operate&p=pipe-hls-timeout&ui=v2");
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
  const audioSearch = inputStatus.getByLabel("Search audio tracks");
  const audioSearchClear = inputStatus.getByRole("button", {
    name: "Clear audio track search",
  });
  await expect(audioSearch).toBeVisible();
  await audioSearch.fill("track 30");
  await expect(
    inputStatus.getByText("Track 30", { exact: true }),
  ).toBeVisible();
  await expect(inputStatus.getByText("Track 6")).toHaveCount(0);
  expect(await getCdpStatusTexts(page)).toContain(
    '1/30 audio tracks match "track 30"',
  );
  await audioSearch.fill("missing audio");
  await expect(
    inputStatus.getByText(
      'No audio tracks match "missing audio". Clear search to show all.',
    ),
  ).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    '0/30 audio tracks match "missing audio"',
  );
  const filteredAudioButtonNames = await getCdpNamesByRole(page, "button");
  expect(filteredAudioButtonNames).toContain("Clear audio track search");
  expect(filteredAudioButtonNames).not.toContain("Clear search");
  await audioSearchClear.click();
  await expect(audioSearch).toHaveValue("");
  await expect(inputStatus.getByText("Track 6")).toBeVisible();
  await expect(inputStatus.getByText("Track 30", { exact: true })).toHaveCount(
    0,
  );
  await expect(audioSearchClear).toBeHidden();
  await expect(
    inputStatus.getByRole("button", { name: "Show all 30 audio tracks" }),
  ).toBeVisible();
  const collapsedAudioButtonNames = await getCdpNamesByRole(page, "button");
  expect(collapsedAudioButtonNames).toContain("Show all 30 audio tracks");
  expect(collapsedAudioButtonNames).not.toContain("Show all 30");
  await inputStatus
    .getByRole("button", { name: "Show all 30 audio tracks" })
    .click();
  await expect(
    inputStatus.getByText("Track 30", { exact: true }),
  ).toBeVisible();
  await expect(
    inputStatus.getByRole("button", { name: "Show fewer audio tracks" }),
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
  const outputToolButtons = await outputOverview
    .locator(
      'button[aria-pressed], button[aria-label^="More output actions for"], button:has-text("Clear output filters")',
    )
    .evaluateAll((buttons) =>
      buttons.map((button) => {
        const rect = button.getBoundingClientRect();
        return {
          height: Math.round(rect.height),
          label:
            button.getAttribute("aria-label") ||
            button.textContent?.replace(/\s+/g, " ").trim() ||
            "(unnamed)",
          width: Math.round(rect.width),
        };
      }),
    );
  expect(outputToolButtons.length).toBeGreaterThan(0);
  for (const button of outputToolButtons) {
    expect(button.height, button.label).toBeGreaterThanOrEqual(36);
    expect(button.width, button.label).toBeGreaterThanOrEqual(44);
  }
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

  await page.goto("/?mode=pipeline&view=operate&p=pipe-retry-budget&ui=v2");
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
