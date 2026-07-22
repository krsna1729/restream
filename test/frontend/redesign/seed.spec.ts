import { expect, test, type Page } from "@playwright/test";

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

test("seed: default empty Overview is v2 and canonical @desktop", async ({
  page,
}) => {
  await openSeededDashboard(page, "empty");

  const overview = page.locator("#dashboard-v2-overview");
  await expect(page).toHaveURL(/\?mode=overview$/);
  await expect(
    overview.getByRole("heading", { name: "Fleet overview" }),
  ).toBeVisible();
  await expect(
    overview.getByRole("cell", { name: "No pipelines configured." }),
  ).toBeVisible();
  await expect(
    overview.getByRole("button", { name: "Add a new pipeline" }),
  ).toBeVisible();
  await expect(page.locator("#dashboard-v2-root")).toBeVisible();
  await expect(page.locator("#overview-mode-content")).toHaveCount(0);
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
  expect(
    await page.evaluate(() =>
      performance
        .getEntriesByType("resource")
        .some((entry) => entry.name.includes("dashboard-v2-entry.js")),
    ),
  ).toBe(true);
});

test("seed: obsolete ui=v1 empty Overview still renders v2 @desktop", async ({
  page,
}) => {
  await openSeededDashboard(page, "empty", "/?mode=overview&ui=v1");

  const overview = page.locator("#dashboard-v2-overview");
  await expect(page).toHaveURL(/\?mode=overview&ui=v1$/);
  await expect(
    overview.getByRole("heading", { name: "Fleet overview" }),
  ).toBeVisible();
  await expect(
    overview.getByRole("cell", { name: "No pipelines configured." }),
  ).toBeVisible();
  await expect(
    overview.getByRole("button", { name: "Add a new pipeline" }),
  ).toBeVisible();
  await expect(page.locator("#dashboard-v2-root")).toBeVisible();
  await expect(page.locator("#overview-mode-content")).toHaveCount(0);
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
  expect(
    await page.evaluate(() =>
      performance
        .getEntriesByType("resource")
        .some((entry) => entry.name.includes("dashboard-v2-entry.js")),
    ),
  ).toBe(true);
});

test("seed: default mixed-health Overview exposes upstream and output state through v2 @desktop", async ({
  page,
}) => {
  await openSeededDashboard(page, "mixed-health");

  const overview = page.locator("#dashboard-v2-overview");
  await expect(
    overview.getByRole("button", { name: "Open pipeline Healthy Program" }),
  ).toBeVisible();
  await expect(
    overview.getByRole("button", {
      name: "Open pipeline Retrying Destination",
    }),
  ).toBeVisible();
  const attention = overview.locator("#dashboard-v2-attention-title");
  await expect(attention).toContainText("1 pipeline needs attention");
  await expect(
    overview.getByRole("heading", { name: "Retrying Destination" }),
  ).toBeVisible();
  await expect(
    overview.getByRole("heading", { name: "Healthy Program" }),
  ).toHaveCount(0);
  await expect(page.locator("#workspace-mode-summary")).toContainText(
    "1 retrying",
  );
});

test("seed: default v2 mounts Settings body in the v2-owned route host @desktop", async ({
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

  await openSeededDashboard(page, "mixed-health", "/?mode=settings", {
    expectOverviewReady: false,
  });
  await expect(page).toHaveURL(/\?mode=settings$/);
  await expect(page.locator("#dashboard-v2-settings-title")).toBeVisible();
  await expect(page.locator("#dashboard-v2-settings-root")).toContainText(
    "Synthetic Restream settings · 5 sections · 3 profiles · 1 auth attempt",
  );
  await expect(
    page.locator(
      '#dashboard-v2-settings-root #dashboard-v2-settings-content[data-dashboard-v2-owned-route-body="settings"]',
    ),
  ).toBeVisible();
  await expect(
    page.locator("#dashboard-v2-settings-root #settings-mode-content"),
  ).toHaveCount(0);
  await expect(page.locator("#settings-mode-panel > #settings-mode-content")).toHaveCount(0);
  await expect(page.locator("#workspace-mode-summary")).toHaveText(
    "Dashboard · Server configuration",
  );
  expect(await getCdpStatusTexts(page)).toContain(
    "Synthetic Restream settings · 5 sections · 3 profiles · 1 auth attempt",
  );
  const settings = page.locator("#dashboard-v2-settings-content");
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
  await expect(page.locator("#dashboard-v2-settings-root")).toContainText(
    "Synthetic Restream settings · 5 sections · 3 profiles · 1 auth attempt",
  );
  await expect(authSearchSummary).toHaveText(
    '1/1 auth attempts match "dashboard"',
  );
  expect(await getCdpStatusTexts(page)).toContain(
    '1/1 auth attempts match "dashboard"',
  );

  await authSearch.fill("banned");
  await expect(page.locator("#dashboard-v2-settings-root")).toContainText(
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
    true,
  );
  const requestsAfterSettings = v2Requests.length;

  await page.goto("/?mode=media");
  await expect(page.locator("#media-mode-panel")).toBeVisible();
  await expect(page.locator("#dashboard-v2-settings-root")).toBeHidden();
  await expect(page.locator("#settings-mode-content")).toHaveCount(0);
  await expect(page.locator("#dashboard-v2-media-title")).toBeVisible();
  await expect(
    page.locator("#dashboard-v2-media-content").getByRole("heading", {
      name: "Media Library",
    }),
  ).toBeVisible();
  await expect(page.locator("#workspace-mode-summary")).toHaveText(
    "Dashboard · Recordings and source files",
  );
  expect(
    v2Requests.some((url) => url.includes("dashboard-v2-checkpoints-entry.js")),
  ).toBe(true);
  expect(v2Requests.some((url) => url.includes("dashboard-v2-entry.js"))).toBe(
    true,
  );
  expect(v2Requests.length).toBeGreaterThanOrEqual(requestsAfterSettings);
  const requestsAfterMedia = v2Requests.length;

  await page.goto("/?mode=status");
  await expect(page.locator("#dashboard-v2-status-title")).toBeVisible();
  await expect(page.locator("#dashboard-v2-media-root")).toBeHidden();
  await expect(page.locator("#media-mode-content")).toHaveCount(0);
  await expect(page.locator("#dashboard-v2-status-root")).toContainText(
    "Status loaded for seeded · commit seeded · 1 process log · 1 notable activity",
  );
  await expect(page.locator("#status-versions")).toContainText("seeded");
  await expect(page.locator("#dashboard-v2-root")).toBeHidden();
  await expect(
    page.locator("#dashboard-v2-pipeline-selector-root"),
  ).toBeHidden();
  await expect(page.locator("#workspace-mode-summary")).toHaveText(
    "Dashboard · Runtime status",
  );
  expect(
    v2Requests.some((url) => url.includes("dashboard-v2-checkpoints-entry.js")),
  ).toBe(true);
  expect(v2Requests.some((url) => url.includes("dashboard-v2-entry.js"))).toBe(
    true,
  );
  expect(v2Requests.length).toBeGreaterThanOrEqual(requestsAfterMedia);

  await page.goto("/?mode=media");
  await expect(
    page.locator("#dashboard-v2-media-content #media-library-results-summary"),
  ).toHaveText("1 media file total · 0 recordings · 1 source file");
  await expect(page.getByText("synthetic-source.mp4")).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    "1 media file total · 0 recordings · 1 source file",
  );

  await page.goto("/?mode=status");
  await expect(page.locator("#dashboard-v2-status-title")).toBeVisible();
  const requestsAfterStatus = v2Requests.length;

  await page.goto("/?mode=pipeline&view=inspect&p=pipe-healthy");
  await expect(page.locator("#inspect-mode-panel")).toBeVisible();
  await expect(
    page.locator("#dashboard-v2-pipeline-selector-root"),
  ).toBeHidden();
  await expect(
    page.locator("#dashboard-v2-pipeline-inspect-title"),
  ).toBeVisible();
  await expect(page.locator("#workspace-mode-summary")).toHaveText(
    "Dashboard · Pipeline graph and diagnostics",
  );
  expect(
    v2Requests.some((url) => url.includes("dashboard-v2-checkpoints-entry.js")),
  ).toBe(true);
  expect(v2Requests.some((url) => url.includes("dashboard-v2-entry.js"))).toBe(
    true,
  );
  expect(v2Requests.length).toBeGreaterThanOrEqual(requestsAfterStatus);
  const requestsAfterInspect = v2Requests.length;

  await page.goto("/?mode=pipeline&view=monitor&p=pipe-healthy");
  await expect(page.locator("#control-mode-panel")).toBeVisible();
  await expect(
    page.locator("#dashboard-v2-pipeline-selector-root"),
  ).toBeHidden();
  await expect(
    page.locator("#dashboard-v2-pipeline-inspect-root"),
  ).toBeHidden();
  await expect(page.locator("#dashboard-v2-control-room-title")).toBeVisible();
  await expect(page.locator("#workspace-mode-summary")).toHaveText(
    "Dashboard · Pipeline monitoring wall",
  );
  expect(v2Requests.length).toBeGreaterThanOrEqual(requestsAfterInspect);
  const requestsAfterMonitor = v2Requests.length;

  await page.goto("/?mode=incidents");
  await expect(page.locator("#incidents-mode-panel")).toBeVisible();
  await expect(page.locator("#dashboard-v2-control-room-root")).toBeHidden();
  await expect(page.locator("#dashboard-v2-incidents-title")).toBeVisible();
  await expect(page.locator("#workspace-mode-summary")).toHaveText(
    "Dashboard · Alerts, evidence, and lifecycle events",
  );
  expect(v2Requests.length).toBeGreaterThanOrEqual(requestsAfterMonitor);
  const requestsAfterIncidents = v2Requests.length;

  await page.goto("/?mode=telemetry");
  await expect(page.locator("#telemetry-mode-panel")).toBeVisible();
  await expect(page.locator("#dashboard-v2-incidents-root")).toBeHidden();
  await expect(page.locator("#dashboard-v2-telemetry-title")).toBeVisible();
  await expect(page.locator("#workspace-mode-summary")).toHaveText(
    "Dashboard · Engine and pipeline counters",
  );
  expect(v2Requests.length).toBeGreaterThanOrEqual(requestsAfterIncidents);
  const requestsAfterTelemetry = v2Requests.length;

  await page.goto("/?mode=pipeline&view=operate");
  await expect(
    page.locator("#dashboard-v2-pipeline-selector-root").getByText("Pipelines"),
  ).toBeVisible();
  expect(v2Requests.length).toBeGreaterThanOrEqual(requestsAfterTelemetry);
  expect(v2Requests.some((url) => url.includes("dashboard-v2-entry.js"))).toBe(
    true,
  );
});

test("seed: default v2 Settings bounds dense auth attempts until requested @desktop", async ({
  page,
}) => {
  await openSeededDashboard(page, "mixed-health", "/?mode=settings", {
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

  const settings = page.locator("#dashboard-v2-settings-content");
  const checkpoint = page.locator("#dashboard-v2-settings-root");
  const authSearch = settings.getByLabel("Search authentication attempts");
  const authSearchSummary = settings.locator("#auth-attempts-search-summary");
  const visibleSettingsControlCount = () =>
    page.evaluate(
      () =>
        Array.from(
          document.querySelectorAll<HTMLElement>(
            "#dashboard-v2-settings-content button,#dashboard-v2-settings-content a[href],#dashboard-v2-settings-content input,#dashboard-v2-settings-content select,#dashboard-v2-settings-content summary,[role='button']",
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
  await expect(page.locator("#dashboard-v2-settings-root")).toContainText(
    "Synthetic Restream settings · 5 sections · 3 profiles · 12 auth attempts",
  );
  await expect(
    checkpoint.getByText(
      "Synthetic Restream settings · 5 sections · 3 profiles · 12 auth attempts",
    ).first(),
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

test("seed: default dashboard mounts route bodies in v2-owned hosts @desktop", async ({
  page,
}) => {
  const routes = [
    {
      hostId: "dashboard-v2-pipeline-inspect-content",
      href: "/?mode=pipeline&view=inspect&p=pipe-retrying",
      nodeBudget: 9_000,
      panelId: "inspect-mode-panel",
      routeKey: "pipeline-inspect",
      rootId: "dashboard-v2-pipeline-inspect-root",
      text: "Inspecting Retrying Destination · input live · 1 output · 1 attention item",
    },
    {
      hostId: "dashboard-v2-control-room-content",
      href: "/?mode=pipeline&view=monitor&p=pipe-retrying",
      nodeBudget: 13_500,
      panelId: "control-mode-panel",
      routeKey: "pipeline-monitor",
      rootId: "dashboard-v2-control-room-root",
      text: "Monitoring Retrying Destination · 1 output · 1 monitor · 0 missing URLs",
    },
    {
      hostId: "dashboard-v2-media-content",
      href: "/?mode=media",
      nodeBudget: 11_500,
      panelId: "media-mode-panel",
      routeKey: "media",
      rootId: "dashboard-v2-media-root",
      text: "1 media file total · 0 recordings · 1 source file",
    },
    {
      hostId: "dashboard-v2-settings-content",
      href: "/?mode=settings",
      nodeBudget: 14_750,
      panelId: "settings-mode-panel",
      routeKey: "settings",
      rootId: "dashboard-v2-settings-root",
      text: "Synthetic Restream settings · 5 sections · 3 profiles · 1 auth attempt",
    },
    {
      hostId: "dashboard-v2-status-content",
      href: "/?mode=status",
      nodeBudget: 16_500,
      panelId: "status-mode-panel",
      routeKey: "status",
      rootId: "dashboard-v2-status-root",
      text: "Status loaded for seeded · commit seeded · 1 process log · 1 notable activity",
    },
    {
      hostId: "dashboard-v2-incidents-content",
      href: "/?mode=incidents",
      nodeBudget: 18_500,
      panelId: "incidents-mode-panel",
      routeKey: "incidents",
      rootId: "dashboard-v2-incidents-root",
      text: "0 critical · 1 warning · 1 recent event · fleet",
    },
    {
      hostId: "dashboard-v2-telemetry-content",
      href: "/?mode=telemetry",
      nodeBudget: 21_500,
      panelId: "telemetry-mode-panel",
      routeKey: "telemetry",
      rootId: "dashboard-v2-telemetry-root",
      text: "Telemetry loaded · 2 ingests · 2 stages · 1 egress · 1 reader · Healthy Program",
    },
  ] as const;

  await openSeededDashboard(page, "mixed-health", routes[0].href, {
    expectOverviewReady: false,
  });

  const routeMetrics: Array<{
    href: string;
    legacyAbsent: boolean;
    nodeBudget: number;
    nodeCount: number;
    noLegacyInsideV2: boolean;
    ownedHostVisible: boolean;
    routeKey: string;
    statusAnnounced: boolean;
  }> = [];

  for (const route of routes) {
    if (page.url() !== new URL(route.href, page.url()).href) {
      await page.goto(route.href);
    }
    const activeRoot = page.locator(`#${route.rootId}`);
    await expect(activeRoot, route.href).toBeVisible();
    await expect(activeRoot, route.href).toContainText(route.text);
    await expect(
      page.locator(
        `#${route.rootId} #${route.hostId}[data-dashboard-v2-owned-route-body="${route.routeKey}"]`,
      ),
      route.href,
    ).toBeVisible();
    await expect(
      page.locator(`#${route.rootId} [id$="-mode-content"]`),
      route.href,
    ).toHaveCount(0);
    await expect(
      page.locator(`#${route.panelId} > [id$="-mode-content"]`),
      route.href,
    ).toHaveCount(0);
    const statusTexts = await getCdpStatusTexts(page);
    expect(statusTexts).toContain(route.text);
    const nodeCount = await getCdpNodeCount(page);
    expect(nodeCount, route.href).toBeLessThan(route.nodeBudget);
    routeMetrics.push({
      href: route.href,
      legacyAbsent: true,
      nodeBudget: route.nodeBudget,
      nodeCount,
      noLegacyInsideV2: true,
      ownedHostVisible: true,
      routeKey: route.routeKey,
      statusAnnounced: statusTexts.includes(route.text),
    });
    for (const otherRoute of routes) {
      const root = page.locator(`#${otherRoute.rootId}`);
      if (otherRoute.rootId === route.rootId) {
        await expect(root, route.href).toBeVisible();
      } else {
        await expect(root, route.href).toBeHidden();
        await expect(
          page.locator(`#${otherRoute.rootId} > *`),
          route.href,
        ).toHaveCount(0);
      }
    }
  }
  console.info(
    `dashboard-v2-default-route-ownership-metrics=${JSON.stringify(routeMetrics)}`,
  );

  await page.goto("/?mode=pipeline&view=inspect&p=pipe-retrying");
  await expect(page.locator("#dashboard-v2-pipeline-inspect-root")).toContainText(
    "Inspecting Retrying Destination · input live · 1 output · 1 attention item",
  );
  await expect(
    page.locator(
      '#dashboard-v2-pipeline-inspect-root #dashboard-v2-pipeline-inspect-content[data-dashboard-v2-owned-route-body="pipeline-inspect"]',
    ),
  ).toBeVisible();

  await page.goto("/?mode=pipeline&view=monitor&p=pipe-retrying");
  await expect(page.locator("#dashboard-v2-control-room-root")).toContainText(
    "Monitoring Retrying Destination · 1 output · 1 monitor · 0 missing URLs",
  );
  await expect(
    page.locator(
      '#dashboard-v2-control-room-root #dashboard-v2-control-room-content[data-dashboard-v2-owned-route-body="pipeline-monitor"]',
    ),
  ).toBeVisible();
});

async function expectStaleRouteCompletionIgnored(
  page: Page,
  route: {
    delay: {
      incidentsDelayMs?: number;
      mediaDelayMs?: number;
      telemetryDelayMs?: number;
    };
    href: string;
    readySelector: string;
    rootId: string;
    routeKey: string;
    staleText: string;
  },
): Promise<void> {
  const delayMs =
    route.delay.incidentsDelayMs ??
    route.delay.mediaDelayMs ??
    route.delay.telemetryDelayMs ??
    0;
  await openSeededDashboard(page, "mixed-health", route.href, {
    expectOverviewReady: false,
    ...route.delay,
  });
  await expect(page.locator(route.readySelector)).toBeVisible();

  await page.locator("#workspace-tab-overview").click();
  await expect(page).toHaveURL(/mode=overview/);
  await expect(page.locator("#workspace-mode-summary")).toContainText(
    "Dashboard",
  );
  await page.waitForTimeout(delayMs + 250);

  const domCounts = await page.evaluate(
    ({ rootId }) => {
      const v2Root = document.getElementById(rootId);
      return {
        legacyBodyCount: document.querySelectorAll('[id$="-mode-content"]').length,
        v2RootChildCount: v2Root?.childElementCount ?? 0,
      };
    },
    {
      rootId: route.rootId,
    },
  );
  const statusTexts = await getCdpStatusTexts(page);
  const metric = {
    ...domCounts,
    cdpNodeCount: await getCdpNodeCount(page),
    overviewStatusAnnounced: statusTexts.some((text) =>
      text.includes("Dashboard"),
    ),
    routeKey: route.routeKey,
  };
  expect(metric.legacyBodyCount, route.routeKey).toBe(0);
  expect(metric.v2RootChildCount, route.routeKey).toBe(0);
  expect(metric.overviewStatusAnnounced, route.routeKey).toBe(true);
  await expect(page.locator("body")).not.toContainText(
    route.staleText,
  );
  console.info(`default-v2-stale-route-guard-metrics=${JSON.stringify(metric)}`);
}

async function waitForBrowserWork(page: Page): Promise<void> {
  await page.evaluate(
    () =>
      new Promise<void>((resolve) => {
        requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
      }),
  );
}

async function expectInactiveRouteEmpty(
  page: Page,
  route: {
    rootId: string;
    routeKey: string;
  },
): Promise<void> {
  const domCounts = await page.evaluate(
    ({ rootId }) => {
      const v2Root = document.getElementById(rootId);
      return {
        legacyBodyCount: document.querySelectorAll('[id$="-mode-content"]').length,
        v2RootChildCount: v2Root?.childElementCount ?? 0,
      };
    },
    {
      rootId: route.rootId,
    },
  );
  const statusTexts = await getCdpStatusTexts(page);
  const metric = {
    ...domCounts,
    cdpNodeCount: await getCdpNodeCount(page),
    overviewStatusAnnounced: statusTexts.some((text) =>
      text.includes("Dashboard"),
    ),
    routeKey: route.routeKey,
  };
  expect(metric.legacyBodyCount, route.routeKey).toBe(0);
  expect(metric.v2RootChildCount, route.routeKey).toBe(0);
  expect(metric.overviewStatusAnnounced, route.routeKey).toBe(true);
  console.info(`default-v2-secondary-stale-guard-metrics=${JSON.stringify(metric)}`);
}

test("seed: default v2 ignores stale Media completions after tab navigation @desktop", async ({
  page,
}) => {
  await expectStaleRouteCompletionIgnored(page, {
    delay: { mediaDelayMs: 700 },
    href: "/?mode=media",
    readySelector: "#dashboard-v2-media-root",
    rootId: "dashboard-v2-media-root",
    routeKey: "media",
    staleText: "synthetic-source.mp4",
  });
});

test("seed: default v2 ignores stale Incidents completions after tab navigation @desktop", async ({
  page,
}) => {
  await expectStaleRouteCompletionIgnored(page, {
    delay: { incidentsDelayMs: 700 },
    href: "/?mode=incidents",
    readySelector: "#dashboard-v2-incidents-root",
    rootId: "dashboard-v2-incidents-root",
    routeKey: "incidents",
    staleText: "Active alerts",
  });
});

test("seed: default v2 ignores stale Telemetry completions after tab navigation @desktop", async ({
  page,
}) => {
  await expectStaleRouteCompletionIgnored(page, {
    delay: { telemetryDelayMs: 700 },
    href: "/?mode=telemetry",
    readySelector: "#dashboard-v2-telemetry-root",
    rootId: "dashboard-v2-telemetry-root",
    routeKey: "telemetry",
    staleText: "Source ring",
  });
});

test("seed: default v2 ignores stale Telemetry stage detail after tab navigation @desktop", async ({
  page,
}) => {
  await openSeededDashboard(page, "mixed-health", "/?mode=telemetry", {
    expectOverviewReady: false,
    stageTelemetryDelayMs: 700,
  });
  await expect(
    page.getByRole("button", { name: "View video telemetry details" }),
  ).toBeVisible();

  const requestPromise = page.waitForRequest(
    (request) =>
      request.method() === "GET" &&
      request.url().includes("/api/v1/stages/") &&
      request.url().endsWith("/telemetry"),
  );
  const responsePromise = page.waitForResponse(
    (response) =>
      response.request().method() === "GET" &&
      response.url().includes("/api/v1/stages/") &&
      response.url().endsWith("/telemetry"),
  );
  await page.getByRole("button", { name: "View video telemetry details" }).click();
  await requestPromise;
  await page.locator("#workspace-tab-overview").click();
  await expect(page).toHaveURL(/mode=overview/);
  await responsePromise;
  await waitForBrowserWork(page);
  await expectInactiveRouteEmpty(page, {
    rootId: "dashboard-v2-telemetry-root",
    routeKey: "telemetry-stage-detail",
  });
});

test("seed: default v2 ignores stale Media delete completion after tab navigation @desktop", async ({
  page,
}) => {
  await openSeededDashboard(page, "mixed-health", "/?mode=media", {
    expectOverviewReady: false,
    mediaDelayMs: 700,
  });
  const sourceRow = page.locator('[data-filename="synthetic-source.mp4"]');
  await sourceRow
    .getByRole("button", {
      name: "Show media actions for synthetic-source.mp4",
    })
    .click();
  await sourceRow
    .getByRole("button", { name: "Delete synthetic-source.mp4" })
    .click();
  const requestPromise = page.waitForRequest(
    (request) =>
      request.method() === "DELETE" &&
      request.url().includes("/api/v1/media/synthetic-source.mp4"),
  );
  const responsePromise = page.waitForResponse(
    (response) =>
      response.request().method() === "DELETE" &&
      response.url().includes("/api/v1/media/synthetic-source.mp4"),
  );
  await page.locator('#app-confirm-dialog button[value="confirm"]').click();
  await requestPromise;
  await page.locator("#workspace-tab-overview").click();
  await expect(page).toHaveURL(/mode=overview/);
  await responsePromise;
  await waitForBrowserWork(page);
  await expectInactiveRouteEmpty(page, {
    rootId: "dashboard-v2-media-root",
    routeKey: "media-delete",
  });
});

test("seed: default v2 ignores stale Monitor input promotion after tab navigation @desktop", async ({
  page,
}) => {
  await openSeededDashboard(
    page,
    "mixed-health",
    "/?mode=pipeline&view=monitor&p=pipe-retrying",
    {
      controlRoomPromotionDelayMs: 700,
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
  const standby = page.locator('article[data-card-id="input:input-standby"]');
  await standby.locator('[data-action="control-room-toggle-card-actions"]').click();
  const requestPromise = page.waitForRequest(
    (request) =>
      request.method() === "POST" &&
      request.url().includes("/inputs/input-standby/promote"),
  );
  const responsePromise = page.waitForResponse(
    (response) =>
      response.request().method() === "POST" &&
      response.url().includes("/inputs/input-standby/promote"),
  );
  await standby.getByRole("button", { name: "Promote", exact: true }).click();
  await requestPromise;
  await page.locator("#workspace-tab-overview").click();
  await expect(page).toHaveURL(/mode=overview/);
  await responsePromise;
  await waitForBrowserWork(page);
  await expectInactiveRouteEmpty(page, {
    rootId: "dashboard-v2-control-room-root",
    routeKey: "monitor-promote",
  });
});

test("seed: default v2 ignores stale Monitor URL save after tab navigation @desktop", async ({
  page,
}) => {
  await openSeededDashboard(
    page,
    "mixed-health",
    "/?mode=pipeline&view=monitor&p=pipe-retrying",
    {
      controlRoomSaveDelayMs: 700,
      expectOverviewReady: false,
    },
  );
  const outputCard = page.locator("article").filter({
    hasText: "Retrying Output",
  });
  await outputCard
    .getByRole("button", { name: "Show monitor actions for Retrying Output" })
    .click();
  await outputCard
    .getByRole("button", { name: "Edit monitoring URL for Retrying Output" })
    .click();
  await outputCard
    .locator('[data-role="control-room-monitoring-input"]')
    .fill("https://monitor.example.invalid/retrying-next");
  const requestPromise = page.waitForRequest(
    (request) =>
      request.method() === "PATCH" &&
      request.url().includes("/outputs/out-retrying"),
  );
  const responsePromise = page.waitForResponse(
    (response) =>
      response.request().method() === "PATCH" &&
      response.url().includes("/outputs/out-retrying"),
  );
  await outputCard.getByRole("button", { name: "Save" }).click();
  await requestPromise;
  await page.locator("#workspace-tab-overview").click();
  await expect(page).toHaveURL(/mode=overview/);
  await responsePromise;
  await waitForBrowserWork(page);
  await expectInactiveRouteEmpty(page, {
    rootId: "dashboard-v2-control-room-root",
    routeKey: "monitor-url-save",
  });
});

test("seed: default v2 unmounts inactive Operate surfaces outside Pipeline @desktop", async ({
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
    "/?mode=pipeline&view=operate&p=pipe-retrying",
    { expectOverviewReady: false },
  );
  await expect(
    page.locator("#dashboard-v2-pipeline-header-root"),
  ).toBeVisible();
  expect(await operateRootChildCount()).toBeGreaterThan(0);

  await page.goto("/?mode=media");
  await expect(
    page.locator("#dashboard-v2-media-content #media-library-results-summary"),
  ).toBeVisible();
  await expect.poll(operateRootChildCount).toBe(0);
  await expect(page.locator("#dashboard-main")).not.toContainText(
    "Start file ingest for Retrying Destination",
  );

  await page.goto("/?mode=pipeline&view=operate&p=pipe-retrying");
  await expect(
    page.locator("#dashboard-v2-pipeline-header-root"),
  ).toBeVisible();
  expect(await operateRootChildCount()).toBeGreaterThan(0);
});

test("seed: dashboard status summary updates while moving across routes @desktop", async ({
  page,
}) => {
  const routes = [
    {
      href: "/?mode=overview",
      text: "Dashboard · 2 live inputs / 1 running outputs / 1 retrying",
    },
    {
      href: "/?mode=pipeline&view=operate&p=pipe-retrying",
      text: "Dashboard · Pipeline workflow",
    },
    {
      href: "/?mode=pipeline&view=inspect&p=pipe-retrying",
      text: "Dashboard · Pipeline graph and diagnostics",
    },
    {
      href: "/?mode=pipeline&view=monitor&p=pipe-retrying",
      text: "Dashboard · Pipeline monitoring wall",
    },
    {
      href: "/?mode=incidents",
      text: "Dashboard · Alerts, evidence, and lifecycle events",
    },
    {
      href: "/?mode=telemetry",
      text: "Dashboard · Engine and pipeline counters",
    },
    {
      href: "/?mode=status",
      text: "Dashboard · Runtime status",
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
  await expect(page.locator("#dashboard-v2-incidents-root")).toContainText(
    "0 critical · 1 warning · 1 recent event · fleet",
  );

  await page.locator("#workspace-tab-telemetry").click();
  await expect(page).toHaveURL(/mode=telemetry/);
  await expect(page.locator("#workspace-tab-telemetry")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(page.locator("#dashboard-v2-telemetry-root")).toContainText(
    "Telemetry loaded · 2 ingests · 3 stages · 2 egresses · 0 readers · fleet",
  );
  expect(await getCdpNodeCount(page)).toBeLessThan(21_000);
});

test("seed: dashboard tablists support arrow key navigation @desktop", async ({
  page,
}) => {
  await openSeededDashboard(page, "mixed-health", "/?mode=overview");

  await page.locator("#workspace-tab-overview").focus();
  await page.keyboard.press("ArrowRight");
  await expect(page).toHaveURL(/mode=pipeline/);
  await expect(page.locator("#workspace-tab-pipeline")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(page.locator("#workspace-mode-summary")).toHaveText(
    "Dashboard · Pipeline workflow",
  );

  await page.keyboard.press("ArrowRight");
  await expect(page).toHaveURL(/mode=incidents/);
  await expect(page.locator("#workspace-tab-incidents")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(page.locator("#dashboard-v2-incidents-root")).toContainText(
    "0 critical · 1 warning · 1 recent event · fleet",
  );

  await page.keyboard.press("ArrowRight");
  await expect(page).toHaveURL(/mode=telemetry/);
  await expect(page.locator("#workspace-tab-telemetry")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(page.locator("#workspace-mode-summary")).toHaveText(
    "Dashboard · Engine and pipeline counters",
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

  await page.goto("/?mode=pipeline&view=operate&p=pipe-retrying");
  const operateTab = page.locator("#pipeline-workspace-tab-operate");
  const inspectTab = page.locator("#pipeline-workspace-tab-inspect");
  const monitorTab = page.locator("#pipeline-workspace-tab-monitor");
  await operateTab.focus();
  await page.keyboard.press("ArrowRight");
  await expect(page).toHaveURL(/view=inspect/);
  await expect(inspectTab).toHaveAttribute("aria-selected", "true");
  await expect(page.locator("#dashboard-v2-pipeline-inspect-root")).toContainText(
    "Inspecting Retrying Destination · input live · 1 output · 1 attention item",
  );
  await page.keyboard.press("ArrowRight");
  await expect(page).toHaveURL(/view=monitor/);
  await expect(monitorTab).toHaveAttribute("aria-selected", "true");
  await expect(page.locator("#dashboard-v2-control-room-root")).toContainText(
    "Monitoring Retrying Destination · 1 output · 1 monitor · 0 missing URLs",
  );
  await page.keyboard.press("ArrowLeft");
  await expect(page).toHaveURL(/view=inspect/);
  await expect(inspectTab).toHaveAttribute("aria-selected", "true");
  expect(await getCdpStatusTexts(page)).toEqual(
    expect.arrayContaining([
      "Dashboard · Pipeline graph and diagnostics",
      "Inspecting Retrying Destination · input live · 1 output · 1 attention item",
    ]),
  );
  expect(await getCdpNodeCount(page)).toBeLessThan(12_000);
});

test("seed: dashboard keeps active tabs visible in narrow rails @desktop", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await openSeededDashboard(page, "mixed-health", "/?mode=telemetry", {
    expectOverviewReady: false,
  });

  await expect(page.locator("#workspace-tab-telemetry")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expectTabVisibleInRail(page, "#workspace-tab-telemetry");
  expect(await getCdpLayoutWidthDelta(page)).toBeLessThanOrEqual(1);

  await page.goto("/?mode=status");
  await expect(page.locator("#workspace-tab-status")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expectTabVisibleInRail(page, "#workspace-tab-status");
  expect(await getCdpLayoutWidthDelta(page)).toBeLessThanOrEqual(1);

  await page.goto("/?mode=pipeline&view=monitor&p=pipe-retrying");
  await expect(page.locator("#pipeline-workspace-tab-monitor")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expectTabVisibleInRail(page, "#pipeline-workspace-tab-monitor");
  await expect(page.locator("#dashboard-v2-control-room-root")).toContainText(
    "Monitoring Retrying Destination · 1 output · 1 monitor · 0 missing URLs",
  );
  expect(await getCdpNodeCount(page)).toBeLessThan(12_000);
  expect(await getCdpLayoutWidthDelta(page)).toBeLessThanOrEqual(1);
});

test("seed: dashboard tolerates operator text zoom without horizontal overflow @desktop", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.addInitScript(() => {
    document.documentElement.style.fontSize = "125%";
  });

  await openSeededDashboard(page, "mixed-health", "/?mode=telemetry", {
    expectOverviewReady: false,
  });

  await expect(page.locator("#workspace-tab-telemetry")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expectTabVisibleInRail(page, "#workspace-tab-telemetry");
  await expect(
    page.locator("#dashboard-v2-telemetry-root").getByRole("heading", {
      name: "Engineer telemetry",
    }),
  ).toBeVisible();
  expect(await getDocumentWidthOverflow(page)).toBeLessThanOrEqual(1);
  expect(await getCdpLayoutWidthDelta(page)).toBeLessThanOrEqual(1);

  await page.goto("/?mode=pipeline&view=monitor&p=pipe-retrying");
  await expect(page.locator("#pipeline-workspace-tab-monitor")).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expectTabVisibleInRail(page, "#pipeline-workspace-tab-monitor");
  await expect(page.locator("#dashboard-v2-control-room-root")).toContainText(
    "Monitoring Retrying Destination · 1 output · 1 monitor · 0 missing URLs",
  );
  expect(await getDocumentWidthOverflow(page)).toBeLessThanOrEqual(1);
  expect(await getCdpLayoutWidthDelta(page)).toBeLessThanOrEqual(1);
});

test("seed: default v2 auth expiry preserves operator return location @desktop", async ({
  page,
}) => {
  const target = "/?mode=pipeline&view=operate&p=pipe-retrying#outputs";
  await openSeededDashboard(page, "mixed-health", target, {
    expectOverviewReady: false,
  });
  await expect(page).toHaveURL(/mode=pipeline/);
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

test("seed: default v2 owned routes keep keyboard and CDP budgets @desktop", async ({
  page,
}) => {
  await openSeededDashboard(page, "mixed-health", "/?mode=overview");

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
  await expect(page).toHaveURL(/mode=pipeline/);
  await expect(page).toHaveURL(/p=pipe-retrying/);
  await expect(page.locator("#dashboard-v2-operate-panel")).toBeVisible();
  await expect(page.locator("#dashboard-v2-operate-panel")).toBeFocused();

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
  await expect(page.locator("#dashboard-v2-pipeline-inspect-root")).toContainText(
    "Inspecting Healthy Program · input live · 1 output · 0 attention items",
  );
  expect(await getCdpNodeCount(page)).toBeLessThan(12_000);
});

test("seed: default v2 skip link reaches main content before dense chrome @desktop", async ({
  page,
}) => {
  await openSeededDashboard(page, "mixed-health", "/?mode=overview");

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
