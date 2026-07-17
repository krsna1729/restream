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

test("seed: ui=v2 keeps legacy routes scoped while checkpoint routes own v2 strips @desktop", async ({
  page,
}) => {
  const v2Requests: string[] = [];
  page.on("request", (request) => {
    if (
      /dashboard-v2-(entry|checkpoints-entry|jsx-runtime)\.js/.test(
        request.url(),
      )
    ) {
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
  await expect(page.locator("#dashboard-v2-settings-title")).toBeVisible();
  await expect(page.locator("#settings-route-summary")).toHaveText(
    "Synthetic Restream settings · 5 sections · 3 profiles · 1 auth attempt",
  );
  await expect(page.locator("#workspace-mode-summary")).toHaveText(
    "UI v2 checkpoint · Server configuration",
  );
  expect(await getCdpStatusTexts(page)).toContain(
    "Synthetic Restream settings · 5 sections · 3 profiles · 1 auth attempt",
  );
  const settings = page.locator("#settings-mode-content");
  const authSearch = settings.getByLabel("Search authentication attempts");
  const authSearchSummary = settings.locator("#auth-attempts-search-summary");
  await expect(authSearchSummary).toHaveText("1 auth attempt visible");
  await expect(
    settings.getByText("Default encryption policy for SRT publishers."),
  ).toBeVisible();
  const initialButtonNames = await getCdpNamesByRole(page, "button");
  expect(initialButtonNames).toEqual(
    expect.arrayContaining(["Save server name", "Save ingest host"]),
  );
  expect(initialButtonNames).not.toContain("Save dashboard password");
  expect(initialButtonNames).not.toContain("Save ingest security settings");
  expect(initialButtonNames).not.toContain("Save");
  await expect(authSearch).toBeHidden();
  await expect(settings.getByLabel("Global SRT ingest mode")).toBeHidden();
  await settings.locator("#srt-settings-section summary").click();
  await expect(settings.locator("#srt-settings-section")).toHaveAttribute(
    "open",
    "",
  );
  await expect(settings.getByLabel("Global SRT ingest mode")).toBeVisible();
  await settings.locator("#transcode-profiles-section summary").click();
  await expect(settings.locator("#transcode-profiles-section")).toHaveAttribute(
    "open",
    "",
  );
  const h264Profile = settings.locator('[data-profile-name="h264"]');
  await expect(h264Profile.getByLabel("h264 preset")).toBeVisible();
  await expect(h264Profile.locator(".js-profile-crf")).toHaveCount(0);
  const collapsedProfileNodes = await getCdpNodeCount(page);
  const showTuning = h264Profile.getByRole("button", {
    name: "Show tuning for h264",
  });
  await expect(showTuning).toHaveAttribute("aria-expanded", "false");
  await showTuning.click();
  await expect(h264Profile.locator(".js-profile-crf")).toBeVisible();
  expect(await getCdpNodeCount(page)).toBeGreaterThan(collapsedProfileNodes);
  const hideTuning = h264Profile.getByRole("button", {
    name: "Hide tuning for h264",
  });
  await expect(hideTuning).toHaveAttribute("aria-expanded", "true");
  await hideTuning.click();
  await expect(h264Profile.locator(".js-profile-crf")).toHaveCount(0);
  let savedProfilePatch: {
    transcodeProfiles?: Record<string, { crf?: number; gop?: number }>;
  } | null = null;
  await page.route("**/api/v1/settings", async (route) => {
    if (route.request().method() !== "PATCH") {
      await route.fallback();
      return;
    }
    savedProfilePatch = route.request().postDataJSON();
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        transcodeProfiles: savedProfilePatch?.transcodeProfiles ?? {},
      }),
    });
  });
  await settings.getByRole("button", { name: "Save Profiles" }).click();
  await expect
    .poll(() => savedProfilePatch?.transcodeProfiles?.h264?.crf)
    .toBe(23);
  expect(savedProfilePatch?.transcodeProfiles?.h264?.gop).toBe(60);

  await settings.locator("#auth-attempts-section > summary").click();
  await expect(authSearch).toBeVisible();
  await authSearch.fill("dashboard");
  await expect(page.locator("#settings-route-summary")).toHaveText(
    "Synthetic Restream settings · 5 sections · 3 profiles · 1 auth attempt",
  );
  await expect(authSearchSummary).toHaveText(
    '1/1 auth attempts match "dashboard"',
  );
  expect(await getCdpStatusTexts(page)).toContain(
    '1/1 auth attempts match "dashboard"',
  );

  await authSearch.fill("banned");
  await expect(page.locator("#settings-route-summary")).toHaveText(
    "Synthetic Restream settings · 5 sections · 3 profiles · 1 auth attempt",
  );
  await expect(authSearchSummary).toHaveText(
    '0/1 auth attempts match "banned"',
  );
  await expect(
    settings.getByText(
      'No authentication attempts match "banned". Clear search to return to the full security log.',
    ),
  ).toBeVisible();
  const clearSearch = settings.getByRole("button", {
    name: "Clear authentication attempt search",
  });
  await expect(clearSearch).toBeVisible();
  const filteredSettingsButtonNames = await getCdpNamesByRole(page, "button");
  expect(filteredSettingsButtonNames).toContain(
    "Clear authentication attempt search",
  );
  expect(filteredSettingsButtonNames).not.toContain("Clear search");
  expect(await getCdpStatusTexts(page)).toContain(
    '0/1 auth attempts match "banned"',
  );

  await clearSearch.click();
  await expect(authSearch).toHaveValue("");
  await expect(authSearchSummary).toHaveText("1 auth attempt visible");
  await expect(clearSearch).toBeHidden();
  await expect(settings.getByRole("cell", { name: "Dashboard" })).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain("1 auth attempt visible");
  await expect(page.locator("#dashboard-v2-root")).toBeHidden();
  await expect(
    page.locator("#dashboard-v2-pipeline-selector-root"),
  ).toBeHidden();
  expect(
    v2Requests.some((url) => url.includes("dashboard-v2-checkpoints-entry.js")),
  ).toBe(true);
  expect(v2Requests.some((url) => url.includes("dashboard-v2-entry.js"))).toBe(
    false,
  );
  const requestsAfterSettings = v2Requests.length;

  await page.goto("/?mode=media&ui=v2");
  await expect(page.locator("#media-mode-panel")).toBeVisible();
  await expect(page.locator("#dashboard-v2-settings-root")).toBeHidden();
  const hiddenSettingsChildCount = await page
    .locator("#settings-mode-content")
    .evaluate((node) => node.childElementCount);
  expect(hiddenSettingsChildCount).toBe(0);
  await expect(page.locator("#dashboard-v2-media-title")).toBeVisible();
  await expect(
    page.locator("#media-mode-content").getByRole("heading", {
      name: "Media Library",
    }),
  ).toBeVisible();
  await expect(page.locator("#workspace-mode-summary")).toHaveText(
    "UI v2 checkpoint · Recordings and source files",
  );
  expect(
    v2Requests.some((url) => url.includes("dashboard-v2-checkpoints-entry.js")),
  ).toBe(true);
  expect(v2Requests.some((url) => url.includes("dashboard-v2-entry.js"))).toBe(
    false,
  );
  expect(v2Requests.length).toBeGreaterThanOrEqual(requestsAfterSettings);
  const requestsAfterMedia = v2Requests.length;

  await page.goto("/?mode=status&ui=v2");
  await expect(page.locator("#dashboard-v2-status-title")).toBeVisible();
  await expect(page.locator("#dashboard-v2-media-root")).toBeHidden();
  const hiddenMediaChildCount = await page
    .locator("#media-mode-content")
    .evaluate((node) => node.childElementCount);
  expect(hiddenMediaChildCount).toBe(0);
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
  await expect(page.locator("#workspace-mode-summary")).toHaveText(
    "UI v2 checkpoint · Runtime status",
  );
  expect(
    v2Requests.some((url) => url.includes("dashboard-v2-checkpoints-entry.js")),
  ).toBe(true);
  expect(v2Requests.some((url) => url.includes("dashboard-v2-entry.js"))).toBe(
    false,
  );
  expect(v2Requests.length).toBeGreaterThanOrEqual(requestsAfterMedia);

  await page.goto("/?mode=media&ui=v2");
  await expect(page.locator("#media-library-results-summary")).toHaveText(
    "1 media file total · 0 recordings · 1 source file",
  );
  await expect(page.getByText("synthetic-source.mp4")).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    "1 media file total · 0 recordings · 1 source file",
  );

  await page.goto("/?mode=status&ui=v2");
  await expect(page.locator("#dashboard-v2-status-title")).toBeVisible();
  const requestsAfterStatus = v2Requests.length;

  await page.goto("/?mode=pipeline&view=inspect&p=pipe-healthy&ui=v2");
  await expect(page.locator("#inspect-mode-panel")).toBeVisible();
  await expect(
    page.locator("#dashboard-v2-pipeline-selector-root"),
  ).toBeHidden();
  await expect(
    page.locator("#dashboard-v2-pipeline-inspect-title"),
  ).toBeVisible();
  await expect(page.locator("#workspace-mode-summary")).toHaveText(
    "UI v2 checkpoint · Pipeline graph and diagnostics",
  );
  expect(
    v2Requests.some((url) => url.includes("dashboard-v2-checkpoints-entry.js")),
  ).toBe(true);
  expect(v2Requests.some((url) => url.includes("dashboard-v2-entry.js"))).toBe(
    false,
  );
  expect(v2Requests.length).toBeGreaterThanOrEqual(requestsAfterStatus);
  const requestsAfterInspect = v2Requests.length;

  await page.goto("/?mode=pipeline&view=monitor&p=pipe-healthy&ui=v2");
  await expect(page.locator("#control-mode-panel")).toBeVisible();
  await expect(
    page.locator("#dashboard-v2-pipeline-selector-root"),
  ).toBeHidden();
  await expect(
    page.locator("#dashboard-v2-pipeline-inspect-root"),
  ).toBeHidden();
  await expect(page.locator("#dashboard-v2-control-room-title")).toBeVisible();
  await expect(page.locator("#workspace-mode-summary")).toHaveText(
    "UI v2 checkpoint · Pipeline monitoring wall",
  );
  expect(v2Requests.length).toBeGreaterThanOrEqual(requestsAfterInspect);
  const requestsAfterMonitor = v2Requests.length;

  await page.goto("/?mode=incidents&ui=v2");
  await expect(page.locator("#incidents-mode-panel")).toBeVisible();
  await expect(page.locator("#dashboard-v2-control-room-root")).toBeHidden();
  await expect(page.locator("#dashboard-v2-incidents-title")).toBeVisible();
  await expect(page.locator("#workspace-mode-summary")).toHaveText(
    "UI v2 checkpoint · Alerts, evidence, and lifecycle events",
  );
  expect(v2Requests.length).toBeGreaterThanOrEqual(requestsAfterMonitor);
  const requestsAfterIncidents = v2Requests.length;

  await page.goto("/?mode=telemetry&ui=v2");
  await expect(page.locator("#telemetry-mode-panel")).toBeVisible();
  await expect(page.locator("#dashboard-v2-incidents-root")).toBeHidden();
  await expect(page.locator("#dashboard-v2-telemetry-title")).toBeVisible();
  await expect(page.locator("#workspace-mode-summary")).toHaveText(
    "UI v2 checkpoint · Engine and pipeline counters",
  );
  expect(v2Requests.length).toBeGreaterThanOrEqual(requestsAfterIncidents);
  const requestsAfterTelemetry = v2Requests.length;

  await page.goto("/?mode=pipeline&view=operate&ui=v2");
  await expect(
    page.locator("#dashboard-v2-pipeline-selector-root").getByText("Pipelines"),
  ).toBeVisible();
  expect(v2Requests.length).toBeGreaterThanOrEqual(requestsAfterTelemetry);
  expect(v2Requests.some((url) => url.includes("dashboard-v2-entry.js"))).toBe(
    true,
  );
});

test("seed: ui=v2 Settings bounds dense auth attempts until requested @desktop", async ({
  page,
}) => {
  await openSeededDashboard(page, "mixed-health", "/?mode=settings&ui=v2", {
    expectOverviewReady: false,
    rateLimitResponse: () => ({
      attempts: Array.from({ length: 12 }, (_, index) => ({
        scope: index % 3 === 0 ? "dashboard-login" : "srt-publish",
        ip: `203.0.113.${10 + index}`,
        failureCount: index + 1,
        banned: index % 4 === 0,
        banRemainingMs: index % 4 === 0 ? 42_000 : undefined,
      })),
    }),
  });

  const settings = page.locator("#settings-mode-content");
  const checkpoint = page.locator("#dashboard-v2-settings-root");
  const authSearch = settings.getByLabel("Search authentication attempts");
  const authSearchSummary = settings.locator("#auth-attempts-search-summary");
  const visibleSettingsControlCount = () =>
    page.evaluate(
      () =>
        Array.from(
          document.querySelectorAll<HTMLElement>(
            "#settings-mode-content button,#settings-mode-content a[href],#settings-mode-content input,#settings-mode-content select,#settings-mode-content summary,[role='button']",
          ),
        ).filter((element) => {
          const rect = element.getBoundingClientRect();
          const style = window.getComputedStyle(element);
          return (
            rect.width > 0 &&
            rect.height > 0 &&
            element.checkVisibility({ checkVisibilityCSS: true }) &&
            style.display !== "none" &&
            style.visibility !== "hidden"
          );
        }).length,
    );
  const visibleServerControlCount = () =>
    page.evaluate(
      () =>
        Array.from(
          document.querySelectorAll<HTMLElement>(
            "#server-settings-section button,#server-settings-section a[href],#server-settings-section input,#server-settings-section select,#server-settings-section summary,[role='button']",
          ),
        ).filter((element) => {
          if (element.closest("[data-settings-v2-disclosure]")) return false;
          const rect = element.getBoundingClientRect();
          const style = window.getComputedStyle(element);
          return (
            rect.width > 0 &&
            rect.height > 0 &&
            element.checkVisibility({ checkVisibilityCSS: true }) &&
            style.display !== "none" &&
            style.visibility !== "hidden"
          );
        }).length,
    );
  await expect(
    checkpoint.locator("#dashboard-v2-settings-title"),
  ).toBeVisible();
  await expect(page.locator("#settings-route-summary")).toHaveText(
    "Synthetic Restream settings · 5 sections · 3 profiles · 12 auth attempts",
  );
  await expect(
    checkpoint.getByText(
      "Synthetic Restream settings · 5 sections · 3 profiles · 12 auth attempts",
    ),
  ).toBeVisible();
  await expect(
    checkpoint.getByText("Security: 3 banned attempts"),
  ).toBeVisible();
  expect(await visibleSettingsControlCount()).toBeLessThanOrEqual(17);
  expect(await visibleServerControlCount()).toBeLessThanOrEqual(5);
  const buttonNames = await getCdpNamesByRole(page, "button");
  expect(buttonNames).toEqual(
    expect.arrayContaining(["Save server name", "Save ingest host"]),
  );
  expect(buttonNames).not.toContain("Save dashboard password");
  expect(buttonNames).not.toContain("Save ingest security settings");
  expect(buttonNames).not.toContain("Refresh authentication attempts");
  expect(buttonNames).not.toContain("Save");
  expect(buttonNames).not.toContain("Refresh");
  expect(buttonNames).not.toContain("Reset");
  const sectionJump = settings.getByLabel("Jump to settings section");
  await expect(sectionJump).toBeVisible();
  await sectionJump.selectOption("srt-settings-section");
  await expect(settings.locator("#srt-settings-section")).toHaveAttribute(
    "open",
    "",
  );
  await expect(page).toHaveURL(/#srt-settings-section$/);
  expect(await getCdpNamesByRole(page, "link")).not.toEqual(
    expect.arrayContaining([
      "Server",
      "Recording",
      "SRT",
      "Backend",
      "Profiles",
    ]),
  );
  expect(await getCdpNamesByRole(page, "link")).not.toEqual(
    expect.arrayContaining([
      "Jump to server settings",
      "Jump to recording settings",
      "Jump to SRT settings",
      "Jump to backend settings",
      "Jump to transcode profile settings",
    ]),
  );
  expect(await getCdpNamesByRole(page, "heading")).toEqual(
    expect.arrayContaining([
      "Settings",
      "Server",
      "Recording",
      "Dashboard Password",
      "Ingest Security",
      "Authentication Attempts",
      "Global SRT Ingest",
      "Transcoding Backend",
      "Transcode Profiles",
    ]),
  );
  await expect(
    settings.locator("[data-settings-v2-disclosure] > summary[aria-label]"),
  ).toHaveCount(7);
  await expect(
    settings.locator('summary[aria-label="Dashboard password settings"]'),
  ).toBeVisible();
  await expect(
    settings.locator('summary[aria-label="Authentication attempt settings"]'),
  ).toBeVisible();
  expect(await getCdpNamesByRole(page, "textbox")).toEqual(
    expect.arrayContaining(["Server name", "Ingest host"]),
  );
  expect(await getCdpNamesByRole(page, "textbox")).not.toEqual(
    expect.arrayContaining(["Name", "e.g. 192.168.1.10 (blank = localhost)"]),
  );
  await expect(authSearch).toBeHidden();
  await settings.locator("#auth-attempts-section > summary").click();
  await expect(authSearch).toBeVisible();
  expect(await getCdpNamesByRole(page, "searchbox")).toContain(
    "Search authentication attempts",
  );
  await expect(authSearchSummary).toHaveText("8 auth attempts shown of 12");
  await expect(
    settings.getByRole("cell", { name: "203.0.113.10" }),
  ).toBeVisible();
  await expect(
    settings.getByRole("cell", { name: "203.0.113.18" }),
  ).toHaveCount(0);
  await expect(
    settings.getByRole("button", { name: "Reset all authentication attempts" }),
  ).toBeHidden();
  await expect(
    settings.getByRole("button", { name: "Reset", exact: true }),
  ).toHaveCount(0);
  const showResetActions = settings.getByRole("button", {
    name: "Show authentication reset actions",
  });
  await expect(showResetActions).toHaveAttribute("aria-expanded", "false");
  await showResetActions.click();
  await expect(
    settings.getByRole("button", { name: "Reset all authentication attempts" }),
  ).toBeVisible();
  const resetButtonNames = await getCdpNamesByRole(page, "button");
  expect(resetButtonNames).toContain("Reset all authentication attempts");
  expect(resetButtonNames).not.toContain("Reset");
  await expect(
    settings
      .getByRole("button", {
        name: "Reset authentication attempt for Dashboard 203.0.113.10",
      })
      .first(),
  ).toBeVisible();
  const hideResetActions = settings.getByRole("button", {
    name: "Hide authentication reset actions",
  });
  await expect(hideResetActions).toHaveAttribute("aria-expanded", "true");
  await hideResetActions.click();
  await expect(
    settings.getByRole("button", { name: "Reset all authentication attempts" }),
  ).toBeHidden();
  await expect(settings.getByRole("button", { name: "Logout" })).toBeHidden();
  const showAccountActions = settings.getByRole("button", {
    name: "Show account actions",
  });
  await expect(showAccountActions).toHaveAttribute("aria-expanded", "false");
  await showAccountActions.click();
  await expect(settings.getByRole("button", { name: "Logout" })).toBeVisible();
  const hideAccountActions = settings.getByRole("button", {
    name: "Hide account actions",
  });
  await expect(hideAccountActions).toHaveAttribute("aria-expanded", "true");
  await hideAccountActions.click();
  await expect(settings.getByRole("button", { name: "Logout" })).toBeHidden();
  const showAll = settings.getByRole("button", {
    name: "Show all 12 authentication attempts",
  });
  await expect(showAll).toHaveAttribute("aria-expanded", "false");
  const denseSettingsButtonNames = await getCdpNamesByRole(page, "button");
  expect(denseSettingsButtonNames).toContain(
    "Show all 12 authentication attempts",
  );
  expect(denseSettingsButtonNames).not.toContain("Show all 12");
  expect(await getCdpStatusTexts(page)).toContain(
    "8 auth attempts shown of 12",
  );
  expect(await getCdpNodeCount(page)).toBeLessThan(13_500);

  await showAll.click();
  await expect(settings.locator("#auth-attempts-toggle")).toHaveAttribute(
    "aria-expanded",
    "true",
  );
  await expect(
    settings.getByRole("cell", { name: "203.0.113.21" }),
  ).toBeVisible();

  await authSearch.fill("203.0.113.21");
  await expect(authSearchSummary).toHaveText(
    '1/12 auth attempts match "203.0.113.21"',
  );
  await expect(
    checkpoint.getByText("1/12 matched", { exact: true }),
  ).toBeVisible();
  await expect(
    settings.getByRole("cell", { name: "203.0.113.21" }),
  ).toBeVisible();
  await expect(
    settings.getByRole("button", { name: /Show (all|fewer)/ }),
  ).toHaveCount(0);
  expect(await getCdpStatusTexts(page)).toContain(
    '1/12 auth attempts match "203.0.113.21"',
  );
  expect(await getCdpNodeCount(page)).toBeLessThan(13_500);
});

test("seed: ui=v2 legacy-owned routes keep operator checkpoints visible and announced @desktop", async ({
  page,
}) => {
  const checkpoints = [
    {
      href: "/?mode=pipeline&view=inspect&p=pipe-retrying&ui=v2",
      locator: "#inspect-route-summary",
      nodeBudget: 9_000,
      rootId: "dashboard-v2-pipeline-inspect-root",
      text: "Inspecting Retrying Destination · input live · 1 output · 1 attention item",
    },
    {
      href: "/?mode=pipeline&view=monitor&p=pipe-retrying&ui=v2",
      locator: "#control-room-route-summary",
      nodeBudget: 9_000,
      rootId: "dashboard-v2-control-room-root",
      text: "Monitoring Retrying Destination · 1 output · 1 monitor · 0 missing URLs",
    },
    {
      href: "/?mode=media&ui=v2",
      locator: "#media-library-results-summary",
      nodeBudget: 11_000,
      rootId: "dashboard-v2-media-root",
      text: "1 media file total · 0 recordings · 1 source file",
    },
    {
      href: "/?mode=settings&ui=v2",
      locator: "#settings-route-summary",
      nodeBudget: 13_500,
      rootId: "dashboard-v2-settings-root",
      text: "Synthetic Restream settings · 5 sections · 3 profiles · 1 auth attempt",
    },
    {
      href: "/?mode=status&ui=v2",
      locator: "#status-route-summary",
      nodeBudget: 16_000,
      rootId: "dashboard-v2-status-root",
      text: "Status loaded for seeded · commit seeded · 1 process log · 1 notable activity",
    },
    {
      href: "/?mode=incidents&ui=v2",
      locator: "#incidents-route-summary",
      nodeBudget: 18_000,
      rootId: "dashboard-v2-incidents-root",
      text: "0 critical · 1 warning · 1 recent event · fleet",
    },
    {
      href: "/?mode=telemetry&ui=v2",
      locator: "#telemetry-route-summary",
      nodeBudget: 21_000,
      rootId: "dashboard-v2-telemetry-root",
      text: "Telemetry loaded · 2 ingests · 2 stages · 1 egress · 1 reader · Healthy Program",
    },
  ] as const;

  await openSeededDashboard(page, "mixed-health", checkpoints[0].href, {
    expectOverviewReady: false,
  });

  for (const checkpoint of checkpoints) {
    if (page.url() !== new URL(checkpoint.href, page.url()).href) {
      await page.goto(checkpoint.href);
    }
    const summary = page.locator(checkpoint.locator);
    await expect(summary).toHaveText(checkpoint.text);
    expect(await getCdpStatusTexts(page)).toContain(checkpoint.text);
    expect(await getCdpNodeCount(page), checkpoint.href).toBeLessThan(
      checkpoint.nodeBudget,
    );
    for (const otherCheckpoint of checkpoints) {
      const root = page.locator(`#${otherCheckpoint.rootId}`);
      if (otherCheckpoint.rootId === checkpoint.rootId) {
        await expect(root, checkpoint.href).toBeVisible();
      } else {
        await expect(root, checkpoint.href).toBeHidden();
        await expect(
          page.locator(`#${otherCheckpoint.rootId} > *`),
          checkpoint.href,
        ).toHaveCount(0);
      }
    }
    if (!checkpoint.href.includes("mode=pipeline")) {
      await expect(
        page.locator(
          "#inspect-mode-panel > :not(#dashboard-v2-pipeline-inspect-root)",
        ),
      ).toHaveCount(0);
      await expect(
        page.locator(
          "#control-mode-panel > :not(#dashboard-v2-control-room-root)",
        ),
      ).toHaveCount(0);
      await expect(page.locator("#inspect-mode-panel h1")).toHaveCount(0);
      await expect(page.locator("#control-mode-panel h1")).toHaveCount(0);
    }
  }

  await page.goto("/?mode=pipeline&view=inspect&p=pipe-retrying&ui=v2");
  await expect(page.locator("#inspect-route-summary")).toHaveText(
    "Inspecting Retrying Destination · input live · 1 output · 1 attention item",
  );
  await expect(
    page.locator("#inspect-mode-panel").getByRole("heading", {
      name: "Pipeline inspect",
    }),
  ).toBeVisible();

  await page.goto("/?mode=pipeline&view=monitor&p=pipe-retrying&ui=v2");
  await expect(page.locator("#control-room-route-summary")).toHaveText(
    "Monitoring Retrying Destination · 1 output · 1 monitor · 0 missing URLs",
  );
  await expect(
    page.locator("#control-mode-panel").getByRole("heading", {
      name: "Control Room",
    }),
  ).toBeVisible();
});

test("seed: ui=v2 unmounts inactive Operate surfaces outside Pipeline @desktop", async ({
  page,
}) => {
  const operateRootChildCount = () =>
    page.evaluate(() =>
      [
        "dashboard-v2-pipeline-selector-root",
        "dashboard-v2-pipeline-header-root",
        "dashboard-v2-pipeline-input-status-root",
        "dashboard-v2-pipeline-output-overview-root",
      ].reduce(
        (sum, id) =>
          sum + (document.getElementById(id)?.childElementCount ?? 0),
        0,
      ),
    );

  await openSeededDashboard(
    page,
    "mixed-health",
    "/?mode=pipeline&view=operate&p=pipe-retrying&ui=v2",
    { expectOverviewReady: false },
  );
  await expect(
    page.locator("#dashboard-v2-pipeline-header-root"),
  ).toBeVisible();
  expect(await operateRootChildCount()).toBeGreaterThan(0);

  await page.goto("/?mode=media&ui=v2");
  await expect(page.locator("#media-library-results-summary")).toBeVisible();
  await expect.poll(operateRootChildCount).toBe(0);
  await expect(page.locator("#dashboard-main")).not.toContainText(
    "Start file ingest for Retrying Destination",
  );

  await page.goto("/?mode=pipeline&view=operate&p=pipe-retrying&ui=v2");
  await expect(
    page.locator("#dashboard-v2-pipeline-header-root"),
  ).toBeVisible();
  expect(await operateRootChildCount()).toBeGreaterThan(0);
});

test("seed: ui=v2 shell announces ownership while moving across routes @desktop", async ({
  page,
}) => {
  const routes = [
    {
      href: "/?mode=overview&ui=v2",
      text: "UI v2 owned · 2 live inputs / 1 running outputs / 1 retrying",
    },
    {
      href: "/?mode=pipeline&view=operate&p=pipe-retrying&ui=v2",
      text: "UI v2 owned · Pipeline workflow",
    },
    {
      href: "/?mode=pipeline&view=inspect&p=pipe-retrying&ui=v2",
      text: "UI v2 checkpoint · Pipeline graph and diagnostics",
    },
    {
      href: "/?mode=pipeline&view=monitor&p=pipe-retrying&ui=v2",
      text: "UI v2 checkpoint · Pipeline monitoring wall",
    },
    {
      href: "/?mode=incidents&ui=v2",
      text: "UI v2 checkpoint · Alerts, evidence, and lifecycle events",
    },
    {
      href: "/?mode=telemetry&ui=v2",
      text: "UI v2 checkpoint · Engine and pipeline counters",
    },
    {
      href: "/?mode=status&ui=v2",
      text: "UI v2 checkpoint · Runtime status",
    },
  ] as const;

  await openSeededDashboard(page, "mixed-health", routes[0].href);

  for (const route of routes) {
    if (page.url() !== new URL(route.href, page.url()).href) {
      await page.goto(route.href);
    }
    await expect(page.locator("#workspace-mode-summary")).toHaveText(
      route.text,
    );
    expect(await getCdpStatusTexts(page)).toContain(route.text);
  }

  await page.locator("#workspace-tab-incidents").click();
  await expect(page).toHaveURL(/mode=incidents/);
  await expect(page.locator("#workspace-tab-incidents")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(page.locator("#incidents-route-summary")).toHaveText(
    "0 critical · 1 warning · 1 recent event · fleet",
  );

  await page.locator("#workspace-tab-telemetry").click();
  await expect(page).toHaveURL(/mode=telemetry/);
  await expect(page.locator("#workspace-tab-telemetry")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(page.locator("#telemetry-route-summary")).toHaveText(
    "Telemetry loaded · 2 ingests · 3 stages · 2 egresses · 0 readers · fleet",
  );
  expect(await getCdpNodeCount(page)).toBeLessThan(21_000);
});

test("seed: ui=v2 shell tablists support arrow key navigation @desktop", async ({
  page,
}) => {
  await openSeededDashboard(page, "mixed-health", "/?mode=overview&ui=v2");

  await page.locator("#workspace-tab-overview").focus();
  await page.keyboard.press("ArrowRight");
  await expect(page).toHaveURL(/mode=pipeline/);
  await expect(page.locator("#workspace-tab-pipeline")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(page.locator("#workspace-mode-summary")).toHaveText(
    "UI v2 owned · Pipeline workflow",
  );

  await page.keyboard.press("ArrowRight");
  await expect(page).toHaveURL(/mode=incidents/);
  await expect(page.locator("#workspace-tab-incidents")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(page.locator("#incidents-route-summary")).toHaveText(
    "0 critical · 1 warning · 1 recent event · fleet",
  );

  await page.keyboard.press("ArrowRight");
  await expect(page).toHaveURL(/mode=telemetry/);
  await expect(page.locator("#workspace-tab-telemetry")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(page.locator("#workspace-mode-summary")).toHaveText(
    "UI v2 checkpoint · Engine and pipeline counters",
  );

  await page.keyboard.press("End");
  await expect(page).toHaveURL(/mode=status/);
  await expect(page.locator("#workspace-tab-status")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await page.keyboard.press("Home");
  await expect(page).toHaveURL(/mode=overview/);
  await expect(page.locator("#workspace-tab-overview")).toHaveAttribute(
    "aria-selected",
    "true",
  );

  await page.goto("/?mode=pipeline&view=operate&p=pipe-retrying&ui=v2");
  const operateTab = page.locator("#pipeline-workspace-tab-operate");
  const inspectTab = page.locator("#pipeline-workspace-tab-inspect");
  const monitorTab = page.locator("#pipeline-workspace-tab-monitor");
  await operateTab.focus();
  await page.keyboard.press("ArrowRight");
  await expect(page).toHaveURL(/view=inspect/);
  await expect(inspectTab).toHaveAttribute("aria-selected", "true");
  await expect(page.locator("#inspect-route-summary")).toHaveText(
    "Inspecting Retrying Destination · input live · 1 output · 1 attention item",
  );
  await page.keyboard.press("ArrowRight");
  await expect(page).toHaveURL(/view=monitor/);
  await expect(monitorTab).toHaveAttribute("aria-selected", "true");
  await expect(page.locator("#control-room-route-summary")).toHaveText(
    "Monitoring Retrying Destination · 1 output · 1 monitor · 0 missing URLs",
  );
  await page.keyboard.press("ArrowLeft");
  await expect(page).toHaveURL(/view=inspect/);
  await expect(inspectTab).toHaveAttribute("aria-selected", "true");
  expect(await getCdpStatusTexts(page)).toEqual(
    expect.arrayContaining([
      "UI v2 checkpoint · Pipeline graph and diagnostics",
      "Inspecting Retrying Destination · input live · 1 output · 1 attention item",
    ]),
  );
  expect(await getCdpNodeCount(page)).toBeLessThan(12_000);
});

test("seed: ui=v2 shell keeps active tabs visible in narrow rails @desktop", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await openSeededDashboard(page, "mixed-health", "/?mode=telemetry&ui=v2", {
    expectOverviewReady: false,
  });

  await expect(page.locator("#workspace-tab-telemetry")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expectTabVisibleInRail(page, "#workspace-tab-telemetry");
  expect(await getCdpLayoutWidthDelta(page)).toBeLessThanOrEqual(1);

  await page.goto("/?mode=status&ui=v2");
  await expect(page.locator("#workspace-tab-status")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expectTabVisibleInRail(page, "#workspace-tab-status");
  expect(await getCdpLayoutWidthDelta(page)).toBeLessThanOrEqual(1);

  await page.goto("/?mode=pipeline&view=monitor&p=pipe-retrying&ui=v2");
  await expect(page.locator("#pipeline-workspace-tab-monitor")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expectTabVisibleInRail(page, "#pipeline-workspace-tab-monitor");
  await expect(page.locator("#control-room-route-summary")).toHaveText(
    "Monitoring Retrying Destination · 1 output · 1 monitor · 0 missing URLs",
  );
  expect(await getCdpNodeCount(page)).toBeLessThan(12_000);
  expect(await getCdpLayoutWidthDelta(page)).toBeLessThanOrEqual(1);
});

test("seed: ui=v2 shell tolerates operator text zoom without horizontal overflow @desktop", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.addInitScript(() => {
    document.documentElement.style.fontSize = "125%";
  });

  await openSeededDashboard(page, "mixed-health", "/?mode=telemetry&ui=v2", {
    expectOverviewReady: false,
  });

  await expect(page.locator("#workspace-tab-telemetry")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expectTabVisibleInRail(page, "#workspace-tab-telemetry");
  await expect(
    page.locator("#telemetry-mode-panel").getByRole("heading", {
      name: "Engineer telemetry",
    }),
  ).toBeVisible();
  expect(await getDocumentWidthOverflow(page)).toBeLessThanOrEqual(1);
  expect(await getCdpLayoutWidthDelta(page)).toBeLessThanOrEqual(1);

  await page.goto("/?mode=pipeline&view=monitor&p=pipe-retrying&ui=v2");
  await expect(page.locator("#pipeline-workspace-tab-monitor")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expectTabVisibleInRail(page, "#pipeline-workspace-tab-monitor");
  await expect(page.locator("#control-room-route-summary")).toHaveText(
    "Monitoring Retrying Destination · 1 output · 1 monitor · 0 missing URLs",
  );
  expect(await getDocumentWidthOverflow(page)).toBeLessThanOrEqual(1);
  expect(await getCdpLayoutWidthDelta(page)).toBeLessThanOrEqual(1);
});

test("seed: ui=v2 auth expiry preserves operator return location @desktop", async ({
  page,
}) => {
  const target = "/?mode=pipeline&view=operate&p=pipe-retrying&ui=v2#outputs";
  await openSeededDashboard(page, "mixed-health", target, {
    expectOverviewReady: false,
  });
  await expect(page).toHaveURL(/mode=pipeline.*ui=v2|ui=v2.*mode=pipeline/);
  const navigations: string[] = [];
  const loginRedirects: string[] = [];
  let expiredRuntimeRequests = 0;
  page.on("framenavigated", (frame) => {
    if (frame === page.mainFrame()) navigations.push(frame.url());
  });
  await page.route("**/login?return=**", async (route) => {
    loginRedirects.push(route.request().url());
    await route.fulfill({
      status: 200,
      contentType: "text/html",
      body: '<!doctype html><form id="login-form"></form>',
    });
  });
  await page.unroute("**/api/v1/**");
  await page.route("**/api/v1/dashboard/runtime**", async (route) => {
    expiredRuntimeRequests += 1;
    await route.fulfill({
      status: 401,
      contentType: "application/json",
      body: JSON.stringify({ error: "login expired" }),
    });
  });

  await page.evaluate(async () => {
    const importModule = new Function("path", "return import(path)") as (
      path: string,
    ) => Promise<{
      getDashboardRuntimeSnapshot: (options: Record<string, unknown>) => void;
    }>;
    const { getDashboardRuntimeSnapshot } =
      await importModule("/js/core/api.js");
    await getDashboardRuntimeSnapshot({
      healthView: "summary",
      metricsView: "summary",
    });
  });
  expect(expiredRuntimeRequests).toBeGreaterThan(0);

  const observedLoginRedirect = () =>
    loginRedirects[0] ??
    navigations.find((url) => url.includes("/login?return="));
  await expect.poll(observedLoginRedirect).toBeTruthy();
  const redirected = new URL(observedLoginRedirect() as string);
  expect(redirected.pathname).toBe("/login");
  expect(redirected.searchParams.get("return")).toBe(target);
});

test("seed: ui=v2 owned routes keep keyboard and CDP budgets @desktop", async ({
  page,
}) => {
  await openSeededDashboard(page, "mixed-health", "/?mode=overview&ui=v2");

  const overview = page.locator("#dashboard-v2-overview");
  await expect(
    overview.getByRole("heading", { name: "Fleet overview" }),
  ).toBeVisible();
  expect(await getCdpNodeCount(page)).toBeLessThan(6_000);

  await page.locator("#workspace-tab-overview").focus();
  const addPipeline = overview.getByRole("button", {
    name: "Add a new pipeline",
    exact: true,
  });
  await tabUntilFocused(page, addPipeline);
  await expect(addPipeline).toBeFocused();

  const attentionCard = overview
    .locator("article")
    .filter({ hasText: "Retrying Destination" });
  const operate = attentionCard.getByRole("button", {
    name: "Operate Retrying Destination",
    exact: true,
  });
  await tabUntilFocused(page, operate);
  await expect(operate).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/mode=pipeline.*ui=v2|ui=v2.*mode=pipeline/);
  await expect(page).toHaveURL(/p=pipe-retrying/);
  await expect(page.locator("#dashboard-grid")).toBeVisible();
  await expect(page.locator("#dashboard-grid")).toBeFocused();

  const selector = page.locator("#dashboard-v2-pipeline-selector-root");
  const header = page.locator("#dashboard-v2-pipeline-header-root");
  const input = page.locator("#dashboard-v2-pipeline-input-status-root");
  const outputs = page.locator("#dashboard-v2-pipeline-output-overview-root");
  await expect(selector).toBeVisible();
  await expect(selector.getByText("Pipelines")).toBeVisible();
  await expect(
    header.getByRole("heading", { name: "Retrying Destination" }),
  ).toBeVisible();
  await expect(
    input.getByRole("heading", { name: "Input and preview" }),
  ).toBeVisible();
  await expect(
    outputs.getByRole("heading", { name: "Output overview" }),
  ).toBeVisible();
  expect(await getCdpNodeCount(page)).toBeLessThan(7_500);

  const pipelineSelect = selector.getByLabel("Select pipeline");
  await expect(pipelineSelect).toBeVisible();
  await pipelineSelect.selectOption("pipe-healthy");
  await expect(page).toHaveURL(/p=pipe-healthy/);
  await expect(
    header.getByRole("heading", { name: "Healthy Program" }),
  ).toBeVisible();
  await expect(
    outputs.getByRole("button", { name: "Stop Healthy Output" }),
  ).toBeVisible();
  expect(await getCdpNamesByRole(page, "button")).toEqual(
    expect.arrayContaining([
      "Stop Healthy Output",
      "More output actions for Healthy Output",
      "Inspect graph for Healthy Program",
      "Diagnose Healthy Program",
    ]),
  );
  expect(await getCdpNamesByRole(page, "button")).not.toEqual(
    expect.arrayContaining([
      "Select pipeline Healthy Program",
      "Select pipeline Retrying Destination",
    ]),
  );
  expect(await getCdpNamesByRole(page, "button")).not.toContain(
    "More actions for Healthy Output",
  );
  expect(await getCdpNamesByRole(page, "button")).not.toEqual(
    expect.arrayContaining([
      "Healthy ProgramLive · 1/1 outputs3.2 Mb/s in2.9 Mb/s out",
      "Retrying DestinationOutput retrying · 0/1 outputs2.4 Mb/s in-- out",
      "Graph",
      "Diagnose",
    ]),
  );

  await header
    .getByRole("button", { name: "Inspect graph for Healthy Program" })
    .click();
  await expect(page).toHaveURL(/view=inspect/);
  await expect(page.locator("#inspect-mode-panel")).toBeVisible();
  await expect(page.locator("#inspect-mode-panel")).toBeFocused();
  await expect(page.locator("#inspect-route-summary")).toHaveText(
    "Inspecting Healthy Program · input live · 1 output · 0 attention items",
  );
  expect(await getCdpNodeCount(page)).toBeLessThan(12_000);
});

test("seed: ui=v2 skip link reaches main content before dense chrome @desktop", async ({
  page,
}) => {
  await openSeededDashboard(page, "mixed-health", "/?mode=overview&ui=v2");

  const skipLink = page.getByRole("link", { name: "Skip to main content" });
  await page.keyboard.press("Tab");
  await expect(skipLink).toBeFocused();
  await expect(skipLink).toBeVisible();
  expect(await getCdpNamesByRole(page, "link")).toContain(
    "Skip to main content",
  );

  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/#dashboard-main$/);
  await expect(page.locator("#overview-mode-panel")).toBeFocused();

  await page.keyboard.press("Tab");
  await expect(
    page
      .locator("#dashboard-v2-overview")
      .getByRole("button", { name: "Add a new pipeline", exact: true }),
  ).toBeFocused();
});
