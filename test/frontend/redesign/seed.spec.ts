import { expect, test, type Locator, type Page } from "@playwright/test";

import { openSeededDashboard } from "./fixtures";

async function getCdpStatusTexts(page: Page): Promise<string[]> {
  const cdp = await page.context().newCDPSession(page);
  const axTree = await cdp.send("Accessibility.getFullAXTree");
  await cdp.detach();
  const axNodeById = new Map(axTree.nodes.map((node) => [node.nodeId, node]));
  return axTree.nodes
    .filter((node) => node.role?.value === "status")
    .map((node) =>
      (node.childIds ?? [])
        .map((childId) => axNodeById.get(childId)?.name?.value)
        .filter(Boolean)
        .join(""),
    );
}

async function getCdpNamesByRole(page: Page, role: string): Promise<string[]> {
  const cdp = await page.context().newCDPSession(page);
  const axTree = await cdp.send("Accessibility.getFullAXTree");
  await cdp.detach();
  return axTree.nodes
    .filter((node) => node.role?.value === role)
    .map((node) => node.name?.value)
    .filter((name): name is string => Boolean(name));
}

async function getCdpNodeCount(page: Page): Promise<number> {
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Performance.enable");
  const performanceMetrics = await cdp.send("Performance.getMetrics");
  await cdp.detach();
  return (
    performanceMetrics.metrics.find((metric) => metric.name === "Nodes")
      ?.value ?? 0
  );
}

async function getCdpLayoutWidthDelta(page: Page): Promise<number> {
  const cdp = await page.context().newCDPSession(page);
  const metrics = await cdp.send("Page.getLayoutMetrics");
  await cdp.detach();
  return metrics.contentSize.width - metrics.cssLayoutViewport.clientWidth;
}

async function getDocumentWidthOverflow(page: Page): Promise<number> {
  return page.evaluate(
    () =>
      document.documentElement.scrollWidth -
      document.documentElement.clientWidth,
  );
}

async function expectTabVisibleInRail(
  page: Page,
  tabSelector: string,
): Promise<void> {
  await expect
    .poll(async () =>
      page.evaluate((selector) => {
        const tab = document.querySelector<HTMLElement>(selector);
        const rail = tab?.closest<HTMLElement>(".dashboard-scrollbar");
        if (!tab || !rail) return false;
        const tabRect = tab.getBoundingClientRect();
        const railRect = rail.getBoundingClientRect();
        return (
          tabRect.left >= railRect.left - 1 &&
          tabRect.right <= railRect.right + 1
        );
      }, tabSelector),
    )
    .toBe(true);
}

async function installPushStateCounter(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const originalPushState = window.history.pushState.bind(window.history);
    const redesignWindow = window as Window & {
      __redesignPushStateCount?: number;
    };
    Object.defineProperty(window, "__redesignPushStateCount", {
      configurable: true,
      value: 0,
      writable: true,
    });
    window.history.pushState = ((...args: Parameters<History["pushState"]>) => {
      redesignWindow.__redesignPushStateCount =
        (redesignWindow.__redesignPushStateCount ?? 0) + 1;
      return originalPushState(...args);
    }) as History["pushState"];
  });
}

async function resetPushStateCounter(page: Page): Promise<void> {
  await page.evaluate(() => {
    (
      window as Window & { __redesignPushStateCount?: number }
    ).__redesignPushStateCount = 0;
  });
}

async function expectPushStateCount(
  page: Page,
  expected: number,
): Promise<void> {
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as Window & { __redesignPushStateCount?: number })
            .__redesignPushStateCount,
      ),
    )
    .toBe(expected);
}

async function tabUntilFocused(
  page: Page,
  locator: Locator,
  maxTabs = 24,
): Promise<void> {
  const focusPath: string[] = [];
  for (let attempt = 0; attempt < maxTabs; attempt += 1) {
    if (await locator.evaluate((node) => node === document.activeElement)) {
      return;
    }
    await page.keyboard.press("Tab");
    focusPath.push(
      await page.evaluate(() => {
        const element = document.activeElement as HTMLElement | null;
        return (
          element?.getAttribute("aria-label") ||
          element?.textContent?.trim().replace(/\s+/g, " ").slice(0, 60) ||
          element?.id ||
          "unknown"
        );
      }),
    );
  }
  throw new Error(`Focus path missed target: ${focusPath.join(" -> ")}`);
}

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
  await expect(
    page.locator("#dashboard-v2-settings-root").getByRole("heading", {
      name: "Settings",
    }),
  ).toBeVisible();
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
  const showTuning = h264Profile.getByRole("button", { name: "Show tuning" });
  await expect(showTuning).toHaveAttribute("aria-expanded", "false");
  await showTuning.click();
  await expect(h264Profile.locator(".js-profile-crf")).toBeVisible();
  expect(await getCdpNodeCount(page)).toBeGreaterThan(collapsedProfileNodes);
  const hideTuning = h264Profile.getByRole("button", { name: "Hide tuning" });
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
    settings.getByText('No authentication attempts match "banned".'),
  ).toBeVisible();
  const clearSearch = settings.getByRole("button", { name: "Clear search" });
  await expect(clearSearch).toBeVisible();
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
  await expect(
    page.locator("#dashboard-v2-media-root").getByRole("heading", {
      name: "Media",
    }),
  ).toBeVisible();
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
  await expect(
    page.locator("#dashboard-v2-status-root").getByRole("heading", {
      name: "Status",
    }),
  ).toBeVisible();
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
  await expect(
    page.locator("#dashboard-v2-status-root").getByRole("heading", {
      name: "Status",
    }),
  ).toBeVisible();
  const requestsAfterStatus = v2Requests.length;

  await page.goto("/?mode=pipeline&view=inspect&p=pipe-healthy&ui=v2");
  await expect(page.locator("#inspect-mode-panel")).toBeVisible();
  await expect(
    page.locator("#dashboard-v2-pipeline-selector-root"),
  ).toBeHidden();
  await expect(
    page
      .locator("#dashboard-v2-pipeline-inspect-root")
      .getByRole("heading", { name: "Healthy Program" }),
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
  await expect(page.locator("#dashboard-v2-pipeline-inspect-root")).toBeHidden();
  await expect(
    page
      .locator("#dashboard-v2-control-room-root")
      .getByRole("heading", { name: "Healthy Program" }),
  ).toBeVisible();
  await expect(page.locator("#workspace-mode-summary")).toHaveText(
    "UI v2 checkpoint · Pipeline monitoring wall",
  );
  expect(v2Requests.length).toBeGreaterThanOrEqual(requestsAfterInspect);
  const requestsAfterMonitor = v2Requests.length;

  await page.goto("/?mode=incidents&ui=v2");
  await expect(page.locator("#incidents-mode-panel")).toBeVisible();
  await expect(page.locator("#dashboard-v2-control-room-root")).toBeHidden();
  await expect(
    page
      .locator("#dashboard-v2-incidents-root")
      .getByRole("heading", { name: "Incidents" }),
  ).toBeVisible();
  await expect(page.locator("#workspace-mode-summary")).toHaveText(
    "UI v2 checkpoint · Alerts, evidence, and lifecycle events",
  );
  expect(v2Requests.length).toBeGreaterThanOrEqual(requestsAfterMonitor);
  const requestsAfterIncidents = v2Requests.length;

  await page.goto("/?mode=telemetry&ui=v2");
  await expect(page.locator("#telemetry-mode-panel")).toBeVisible();
  await expect(page.locator("#dashboard-v2-incidents-root")).toBeHidden();
  await expect(
    page
      .locator("#dashboard-v2-telemetry-root")
      .getByRole("heading", { name: "Engineer telemetry" }),
  ).toBeVisible();
  await expect(page.locator("#workspace-mode-summary")).toHaveText(
    "UI v2 checkpoint · Engine and pipeline counters",
  );
  expect(v2Requests.length).toBeGreaterThanOrEqual(requestsAfterIncidents);
  const requestsAfterTelemetry = v2Requests.length;

  await page.goto("/?mode=pipeline&view=operate&ui=v2");
  await expect(
    page
      .locator("#dashboard-v2-pipeline-selector-root")
      .getByRole("heading", { name: "Pipelines" }),
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
  await expect(checkpoint.getByRole("heading", { name: "Settings" })).toBeVisible();
  await expect(page.locator("#settings-route-summary")).toHaveText(
    "Synthetic Restream settings · 5 sections · 3 profiles · 12 auth attempts",
  );
  await expect(
    checkpoint.getByText(
      "Synthetic Restream settings · 5 sections · 3 profiles · 12 auth attempts",
    ),
  ).toBeVisible();
  await expect(checkpoint.getByText("Security: 3 banned attempts")).toBeVisible();
  await expect(authSearchSummary).toHaveText("8 auth attempts shown of 12");
  await expect(
    settings.getByRole("cell", { name: "203.0.113.10" }),
  ).toBeVisible();
  await expect(
    settings.getByRole("cell", { name: "203.0.113.18" }),
  ).toHaveCount(0);
  await expect(
    settings.getByRole("button", { name: "Reset All" }),
  ).toBeHidden();
  await expect(
    settings.getByRole("button", { name: "Reset", exact: true }),
  ).toHaveCount(0);
  const showResetActions = settings.getByRole("button", {
    name: "Show reset actions",
  });
  await expect(showResetActions).toHaveAttribute("aria-expanded", "false");
  await showResetActions.click();
  await expect(
    settings.getByRole("button", { name: "Reset All" }),
  ).toBeVisible();
  await expect(
    settings.getByRole("button", { name: "Reset", exact: true }).first(),
  ).toBeVisible();
  const hideResetActions = settings.getByRole("button", {
    name: "Hide reset actions",
  });
  await expect(hideResetActions).toHaveAttribute("aria-expanded", "true");
  await hideResetActions.click();
  await expect(
    settings.getByRole("button", { name: "Reset All" }),
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
  const showAll = settings.getByRole("button", { name: "Show all 12" });
  await expect(showAll).toHaveAttribute("aria-expanded", "false");
  expect(await getCdpStatusTexts(page)).toContain("8 auth attempts shown of 12");
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
  await expect(checkpoint.getByText("1/12 matched", { exact: true })).toBeVisible();
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
      nodeBudget: 6_000,
      text: "Inspecting Retrying Destination · input live · 1 output · 1 attention item",
    },
    {
      href: "/?mode=pipeline&view=monitor&p=pipe-retrying&ui=v2",
      locator: "#control-room-route-summary",
      nodeBudget: 8_500,
      text: "Monitoring Retrying Destination · 1 output · 1 monitor · 0 missing URLs",
    },
    {
      href: "/?mode=media&ui=v2",
      locator: "#media-library-results-summary",
      nodeBudget: 10_500,
      text: "1 media file total · 0 recordings · 1 source file",
    },
    {
      href: "/?mode=settings&ui=v2",
      locator: "#settings-route-summary",
      nodeBudget: 13_500,
      text: "Synthetic Restream settings · 5 sections · 3 profiles · 1 auth attempt",
    },
    {
      href: "/?mode=status&ui=v2",
      locator: "#status-route-summary",
      nodeBudget: 16_000,
      text: "Status loaded for seeded · commit seeded · 1 process log · 1 notable activity",
    },
    {
      href: "/?mode=incidents&ui=v2",
      locator: "#incidents-route-summary",
      nodeBudget: 18_000,
      text: "0 critical · 1 warning · 1 recent event · fleet",
    },
    {
      href: "/?mode=telemetry&ui=v2",
      locator: "#telemetry-route-summary",
      nodeBudget: 21_000,
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
    if (!checkpoint.href.includes("mode=pipeline")) {
      await expect(
        page.locator(
          "#inspect-mode-panel > :not(#dashboard-v2-pipeline-inspect-root)",
        ),
      ).toHaveCount(0);
      await expect(
        page.locator("#control-mode-panel > :not(#dashboard-v2-control-room-root)"),
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
  const target =
    "/?mode=pipeline&view=operate&p=pipe-retrying&ui=v2#outputs";
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
    const importModule = new Function(
      "path",
      "return import(path)",
    ) as (path: string) => Promise<{
      getDashboardRuntimeSnapshot: (options: Record<string, unknown>) => void;
    }>;
    const { getDashboardRuntimeSnapshot } = await importModule(
      "/js/core/api.js",
    );
    await getDashboardRuntimeSnapshot({
      healthView: "summary",
      metricsView: "summary",
    });
  });
  expect(expiredRuntimeRequests).toBeGreaterThan(0);

  const observedLoginRedirect = () =>
    loginRedirects[0] ??
    navigations.find((url) => url.includes("/login?return="));
  await expect
    .poll(
      observedLoginRedirect,
    )
    .toBeTruthy();
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
    name: "Add Pipeline",
    exact: true,
  });
  await tabUntilFocused(page, addPipeline);
  await expect(addPipeline).toBeFocused();

  const attentionCard = overview
    .locator("article")
    .filter({ hasText: "Retrying Destination" });
  const operate = attentionCard.getByRole("button", {
    name: "Operate",
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
  await expect(
    selector.getByRole("heading", { name: "Pipelines" }),
  ).toBeVisible();
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

  await selector.getByRole("button", { name: /Healthy Program/ }).focus();
  await selector
    .getByRole("button", { name: /Healthy Program/ })
    .press("Enter");
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
      "More actions for Healthy Output",
      "Graph",
      "Diagnose",
    ]),
  );

  await header.getByRole("button", { name: "Graph" }).click();
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
      .getByRole("button", { name: "Add Pipeline", exact: true }),
  ).toBeFocused();
});

test("seed: ui=v2 overview Operate is one predictable history step @desktop", async ({
  page,
}) => {
  await installPushStateCounter(page);
  await openSeededDashboard(page, "mixed-health", "/?mode=overview&ui=v2");
  await resetPushStateCounter(page);

  await page
    .locator("#dashboard-v2-overview")
    .locator("article")
    .filter({ hasText: "Retrying Destination" })
    .getByRole("button", { name: "Operate", exact: true })
    .click();
  await expect(page).toHaveURL(/mode=pipeline.*ui=v2|ui=v2.*mode=pipeline/);
  await expect(page).toHaveURL(/view=operate/);
  await expect(page).toHaveURL(/p=pipe-retrying/);
  await expect(
    page.locator("#dashboard-v2-pipeline-header-root").getByRole("heading", {
      name: "Retrying Destination",
    }),
  ).toBeVisible();
  await expectPushStateCount(page, 1);

  await page.goBack();
  await expect(page).toHaveURL(/\?mode=overview&ui=v2$/);
  await expect(
    page
      .locator("#dashboard-v2-overview")
      .getByRole("heading", { name: "Fleet overview" }),
  ).toBeVisible();
  expect(await getCdpNodeCount(page)).toBeLessThan(6_000);
});

test("seed: ui=v2 overview Inspect is one predictable history step @desktop", async ({
  page,
}) => {
  await installPushStateCounter(page);
  await openSeededDashboard(page, "mixed-health", "/?mode=overview&ui=v2", {
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
    .getByRole("button", { name: "Inspect", exact: true })
    .click();
  await expect(page).toHaveURL(/mode=pipeline.*ui=v2|ui=v2.*mode=pipeline/);
  await expect(page).toHaveURL(/view=inspect/);
  await expect(page).toHaveURL(/p=pipe-retrying/);
  await expect(page.locator("#inspect-mode-panel")).toBeVisible();
  await expect(
    page.locator("#inspect-mode-panel").getByRole("heading", {
      name: "Pipeline inspect",
    }),
  ).toBeVisible();
  await expect(page.locator("#inspect-pipeline-select")).toHaveValue(
    "pipe-retrying",
  );
  await expect(page.locator("#inspect-route-summary")).toHaveText(
    "Inspecting Retrying Destination · input live · 1 output · 1 attention item",
  );
  const inspectCheckpoint = page.locator("#dashboard-v2-pipeline-inspect-root");
  await expect(
    inspectCheckpoint.getByRole("heading", { name: "Retrying Destination" }),
  ).toBeVisible();
  await expect(inspectCheckpoint.getByText("Output retrying")).toBeVisible();
  await expect(
    inspectCheckpoint.getByText("1 fault candidate", { exact: true }),
  ).toBeVisible();
  await expect(
    inspectCheckpoint.getByRole("button", { name: "Operate" }),
  ).toBeEnabled();
  await expect(
    inspectCheckpoint.getByRole("button", { name: "Diagnostics" }),
  ).toBeEnabled();
  await expect(page.locator("#inspect-focus-summary")).toHaveText(
    "Inspection focus · 1 blocker before active probes · 1 fault candidate · Inspect recent errors and retry backoff before forcing a restart.",
  );
  expect(await getCdpStatusTexts(page)).toContain(
    "Inspecting Retrying Destination · input live · 1 output · 1 attention item",
  );
  expect(await getCdpStatusTexts(page)).toContain(
    "Inspection focus · 1 blocker before active probes · 1 fault candidate · Inspect recent errors and retry backoff before forcing a restart.",
  );
  const resourceDetails = page.locator("#inspect-resource-details");
  await expect(resourceDetails.getByText("Process Metrics")).toBeVisible();
  await expect(resourceDetails.getByText("Pipeline Attribution")).toBeVisible();
  await expect(
    resourceDetails.getByRole("button", { name: "Show resource details" }),
  ).toHaveAttribute("aria-expanded", "false");
  await expect(resourceDetails.getByText("FFmpeg workers")).toHaveCount(0);
  await resourceDetails
    .getByRole("button", { name: "Show resource details" })
    .click();
  await expect(
    resourceDetails.getByRole("button", { name: "Hide resource details" }),
  ).toHaveAttribute("aria-expanded", "true");
  await expect(resourceDetails.getByText("FFmpeg workers")).toBeVisible();
  await expect(resourceDetails.getByText("video:720p")).toBeVisible();
  await resourceDetails
    .getByRole("button", { name: "Hide resource details" })
    .click();
  await expect(resourceDetails.getByText("FFmpeg workers")).toHaveCount(0);
  await expectPushStateCount(page, 1);

  await page.goBack();
  await expect(page).toHaveURL(/\?mode=overview&ui=v2$/);
  await expect(
    page
      .locator("#dashboard-v2-overview")
      .getByRole("heading", { name: "Fleet overview" }),
  ).toBeVisible();
  expect(await getCdpNodeCount(page)).toBeLessThan(6_000);
});

test("seed: ui=v2 Inspect output search narrows noisy sibling outputs @desktop", async ({
  page,
}) => {
  await openSeededDashboard(
    page,
    "chaos-recovery",
    "/?mode=pipeline&view=inspect&p=pipe-stall&ui=v2",
    { expectOverviewReady: false },
  );

  const inspect = page.locator("#inspect-mode-panel");
  await expect(page.locator("#inspect-route-summary")).toHaveText(
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

test("seed: ui=v2 Inspect output search understands down aliases @desktop", async ({
  page,
}) => {
  await openSeededDashboard(
    page,
    "chaos-recovery",
    "/?mode=pipeline&view=inspect&p=pipe-stall&ui=v2",
    {
      expectOverviewReady: false,
      runtimeResponse: (runtime) => {
        const next = structuredClone(runtime);
        const pipeline = (
          next.health as Record<string, Record<string, unknown>>
        ).pipelines?.["pipe-stall"] as
          | { outputs?: Record<string, Record<string, unknown>> }
          | undefined;
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
  await expect(page.locator("#inspect-route-summary")).toHaveText(
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

test("seed: ui=v2 pipeline workspace tabs preserve one selected context @desktop", async ({
  page,
}) => {
  await installPushStateCounter(page);
  await openSeededDashboard(
    page,
    "mixed-health",
    "/?mode=pipeline&view=operate&p=pipe-retrying&ui=v2",
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

test("seed: ui=v2 Monitor search does not mislabel filtered outputs as missing @desktop", async ({
  page,
}) => {
  await openSeededDashboard(
    page,
    "mixed-health",
    "/?mode=pipeline&view=monitor&p=pipe-retrying&ui=v2",
    { expectOverviewReady: false },
  );

  const monitor = page.locator("#control-mode-panel");
  const checkpoint = page.locator("#dashboard-v2-control-room-root");
  const search = monitor.locator("#control-room-search-input");
  const routeSummary = monitor.locator("#control-room-route-summary");
  const summary = monitor.locator("#control-room-summary");
  await expect(
    monitor.getByRole("heading", { name: "Control Room" }),
  ).toBeVisible();
  await expect(routeSummary).toHaveText(
    "Monitoring Retrying Destination · 1 output · 1 monitor · 0 missing URLs",
  );
  expect(await getCdpStatusTexts(page)).toContain(
    "Monitoring Retrying Destination · 1 output · 1 monitor · 0 missing URLs",
  );
  await expect(
    checkpoint.getByRole("heading", { name: "Retrying Destination" }),
  ).toBeVisible();
  await expect(checkpoint.getByText("1/1 monitored")).toBeVisible();
  await expect(checkpoint.getByText("No active search")).toBeVisible();
  await expect(summary).toHaveText(
    "1/1 monitored · 0 missing monitoring URLs",
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
  const clearSearch = monitor.getByRole("button", { name: "Clear search" });
  await expect(clearSearch).toBeVisible();
  expect(await getCdpStatusTexts(page)).toEqual(
    expect.arrayContaining([
      '0/1 monitored match · 0 missing monitoring URLs · "nowhere"',
    ]),
  );

  await clearSearch.click();
  await expect(search).toHaveValue("");
  await expect(summary).toHaveText(
    "1/1 monitored · 0 missing monitoring URLs",
  );
  await expect(checkpoint.getByText("No active search")).toBeVisible();
  await expect(clearSearch).toBeHidden();
  await expect(monitor.getByText("Retrying Output")).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    "1/1 monitored · 0 missing monitoring URLs",
  );
  expect(await getCdpNodeCount(page)).toBeLessThan(7_500);
});

test("seed: ui=v2 Monitor search understands operator status terms @desktop", async ({
  page,
}) => {
  await openSeededDashboard(
    page,
    "chaos-recovery",
    "/?mode=pipeline&view=monitor&p=pipe-retry-budget&ui=v2",
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
  await expect(monitor.locator("#control-room-route-summary")).toHaveText(
    "Monitoring Retry Budget Exhausted · 2 outputs · 2 monitors · 0 missing URLs",
  );
  await expect(checkpoint.getByText("2 monitors down")).toBeVisible();
  await expect(summary).toHaveText(
    "2/2 monitored · 0 missing monitoring URLs",
  );

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

test("seed: ui=v2 Monitor lazily loads generic web previews @desktop", async ({
  page,
}) => {
  await openSeededDashboard(
    page,
    "chaos-recovery",
    "/?mode=pipeline&view=monitor&p=pipe-flapping&ui=v2",
    { expectOverviewReady: false },
  );

  const monitor = page.locator("#control-mode-panel");
  const checkpoint = page.locator("#dashboard-v2-control-room-root");
  await expect(monitor.locator("#control-room-route-summary")).toHaveText(
    "Monitoring Recovered Sink Flap · 1 output · 1 monitor · 0 missing URLs",
  );
  await expect(
    checkpoint.getByRole("heading", { name: "Recovered Sink Flap" }),
  ).toBeVisible();
  await expect(
    checkpoint.getByText("1 lazy web preview", { exact: true }).first(),
  ).toBeVisible();
  await expect(
    monitor.getByRole("button", { name: "Load preview" }),
  ).toBeVisible();
  const outputCard = monitor.locator("article").filter({
    hasText: "SRT Sink Flap",
  });
  await expect(
    outputCard.getByRole("button", { name: "Edit" }),
  ).toBeHidden();
  await expect(
    outputCard.getByRole("button", { name: "Copy" }),
  ).toBeHidden();
  const showActions = outputCard.getByRole("button", {
    name: "Show monitor actions",
  });
  await expect(showActions).toBeVisible();
  await expect(showActions).toHaveAttribute("aria-expanded", "false");
  await showActions.click();
  await expect(
    outputCard.getByRole("button", { name: "Edit" }),
  ).toBeVisible();
  await expect(
    outputCard.getByRole("button", { name: "Copy" }),
  ).toBeVisible();
  await expect(
    outputCard.getByRole("button", { name: "Open" }),
  ).toBeVisible();
  const hideActions = outputCard.getByRole("button", {
    name: "Hide monitor actions",
  });
  await expect(hideActions).toHaveAttribute("aria-expanded", "true");
  await hideActions.click();
  await expect(
    outputCard.getByRole("button", { name: "Edit" }),
  ).toBeHidden();
  await expect(monitor.locator("iframe")).toHaveCount(0);
  expect(await getCdpNodeCount(page)).toBeLessThan(7_500);

  await monitor.getByRole("button", { name: "Load preview" }).click();
  await expect(monitor.locator("iframe")).toHaveCount(1);
});

test("seed: ui=v2 Monitor lazily loads HLS output previews @desktop", async ({
  page,
}) => {
  await openSeededDashboard(
    page,
    "mixed-health",
    "/?mode=pipeline&view=monitor&p=pipe-retrying&ui=v2",
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
        const output = (
          next.health as Record<string, Record<string, unknown>>
        ).pipelines?.["pipe-retrying"] as
          | { outputs?: Record<string, Record<string, unknown>> }
          | undefined;
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
  await expect(monitor.locator("#control-room-route-summary")).toHaveText(
    "Monitoring Retrying Destination · 1 output · 1 monitor · 0 missing URLs",
  );
  await expect(
    monitor.getByRole("button", { name: "Load preview" }),
  ).toBeVisible();
  const outputCard = monitor.locator("article").filter({
    hasText: "Retrying Output",
  });
  await expect(
    outputCard.locator('[data-role="managed-hls-video"]'),
  ).toHaveCount(0);
  await expect(outputCard.locator("video")).toHaveCount(0);
  expect(await getCdpNodeCount(page)).toBeLessThan(7_500);

  await outputCard.getByRole("button", { name: "Load preview" }).click();
  await expect(
    outputCard.locator('[data-role="managed-hls-video"]'),
  ).toHaveCount(1);
});

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
  await expect(checkpoint.getByRole("heading", { name: "Media" })).toBeVisible();
  await expect(
    media.getByRole("heading", { name: "Media Library" }),
  ).toBeVisible();
  await expect(summary).toHaveText(
    "1 media file total · 0 recordings · 1 source file",
  );
  await expect(
    checkpoint.getByText("1 media file total · 0 recordings · 1 source file"),
  ).toBeVisible();
  await expect(checkpoint.getByText("1 source file", { exact: true })).toBeVisible();
  const sourceRow = media.locator('[data-filename="synthetic-source.mp4"]');
  await expect(sourceRow.getByRole("link", { name: "Download" })).toHaveCount(0);
  await expect(
    sourceRow.getByRole("button", {
      name: "Show actions for synthetic-source.mp4",
    }),
  ).toHaveAttribute("aria-expanded", "false");
  await expect(sourceRow.getByRole("button", { name: "Rename" })).toHaveCount(0);
  await expect(sourceRow.getByRole("button", { name: "Delete" })).toHaveCount(0);
  await sourceRow
    .getByRole("button", { name: "Show actions for synthetic-source.mp4" })
    .click();
  await expect(
    sourceRow.getByRole("button", {
      name: "Hide actions for synthetic-source.mp4",
    }),
  ).toHaveAttribute("aria-expanded", "true");
  await expect(sourceRow.getByRole("link", { name: "Download" })).toBeVisible();
  await expect(sourceRow.getByRole("button", { name: "Rename" })).toBeVisible();
  await expect(sourceRow.getByRole("button", { name: "Delete" })).toBeVisible();
  await sourceRow
    .getByRole("button", { name: "Hide actions for synthetic-source.mp4" })
    .click();
  await expect(sourceRow.getByRole("link", { name: "Download" })).toHaveCount(0);
  await expect(sourceRow.getByRole("button", { name: "Rename" })).toHaveCount(0);

  await search.fill("synthetic");
  await expect(summary).toHaveText(
    '1/1 media file shown · 0 recordings · 1 source file matched · "synthetic"',
  );
  await expect(
    checkpoint.getByText(
      '1/1 media file shown · 0 recordings · 1 source file matched · "synthetic"',
    ),
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
    ),
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
  const clearSearch = media.getByRole("button", { name: "Clear search" });
  await expect(clearSearch).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    '0/1 media files shown · 0 recordings · 0 source files matched · "missing"',
  );

  await clearSearch.click();
  await expect(search).toHaveValue("");
  await expect(summary).toHaveText(
    "1 media file total · 0 recordings · 1 source file",
  );
  await expect(
    checkpoint.getByText("1 media file total · 0 recordings · 1 source file"),
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
          convertedName:
            index === 11 ? "dense-recording-12.mp4" : undefined,
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
    checkpoint.getByText("26 media files total · 12 recordings · 14 source files"),
  ).toBeVisible();
  await expect(checkpoint.getByText("12 recordings", { exact: true })).toBeVisible();
  await expect(checkpoint.getByText("14 source files", { exact: true })).toBeVisible();
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
    name: "Show all 12",
  });
  const showAllSources = media.getByRole("button", { name: "Show all 14" });
  await expect(showAllRecordings).toHaveAttribute("aria-expanded", "false");
  await expect(showAllSources).toHaveAttribute("aria-expanded", "false");
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
    ),
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
  await expect(checkpoint.getByRole("heading", { name: "Status" })).toBeVisible();
  await expect(
    status.locator("#status-mode-content").getByRole("heading", {
      name: "Status",
    }),
  ).toBeVisible();
  await expect(status.locator("#status-route-summary")).toHaveText(
    "Status loaded for seeded · commit seeded · 1 process log · 1 notable activity",
  );
  await expect(checkpoint.getByText("seeded · seeded", { exact: true })).toBeVisible();
  await expect(checkpoint.getByText("1 process log", { exact: true })).toBeVisible();
  await expect(
    checkpoint.getByText("1 notable activity", { exact: true }),
  ).toBeVisible();
  await expect(
    status.getByRole("button", { name: "Show Toolchain details" }),
  ).toBeVisible();
  await expect(status.locator("#status-toolchain-section table")).toHaveCount(0);
  await status.getByRole("button", { name: "Show Toolchain details" }).click();
  await expect(status.locator("#status-toolchain-section table")).toHaveCount(1);
  await expect(status.locator("#status-toolchain-section")).toContainText(
    "Target",
  );
  await status.getByRole("button", { name: "Hide Toolchain details" }).click();
  await expect(status.locator("#status-toolchain-section table")).toHaveCount(0);
  await expect(
    status.getByRole("button", { name: "Download Status" }),
  ).toBeHidden();
  const exportActions = status.getByRole("button", {
    name: "Show export actions",
  });
  await expect(exportActions).toBeVisible();
  await expect(exportActions).toHaveAttribute("aria-expanded", "false");
  await exportActions.click();
  await expect(
    status.getByRole("button", { name: "Download Status" }),
  ).toBeVisible();
  await expect(status.getByRole("button", { name: "Copy SBOM" })).toBeVisible();
  const hideExportActions = status.getByRole("button", {
    name: "Hide export actions",
  });
  await expect(hideExportActions).toHaveAttribute("aria-expanded", "true");
  await hideExportActions.click();
  await expect(
    status.getByRole("button", { name: "Download Status" }),
  ).toBeHidden();
  expect(await getCdpStatusTexts(page)).toContain(
    "Status loaded for seeded · commit seeded · 1 process log · 1 notable activity",
  );
  const search = status.getByLabel("Search process logs and activity");
  const searchSummary = status.locator("#status-log-search-results-summary");
  await expect(searchSummary).toHaveText("1 activity · 1 process log visible");

  await search.fill("synthetic");
  await expect(status.locator("#status-route-summary")).toHaveText(
    "Status loaded for seeded · commit seeded · 1 process log · 1 notable activity",
  );
  await expect(searchSummary).toHaveText(
    '1 activity · 1 process log match "synthetic"',
  );
  await expect(
    checkpoint.getByText('1 activity · 1 process log match "synthetic"'),
  ).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    '1 activity · 1 process log match "synthetic"',
  );

  await search.fill("missing");
  await expect(status.locator("#status-route-summary")).toHaveText(
    "Status loaded for seeded · commit seeded · 1 process log · 1 notable activity",
  );
  await expect(searchSummary).toHaveText(
    '0 activities · 0 process logs match "missing"',
  );
  await expect(
    checkpoint.getByText('0 activities · 0 process logs match "missing"'),
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
  const clearSearch = status.getByRole("button", { name: "Clear search" });
  await expect(clearSearch).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    '0 activities · 0 process logs match "missing"',
  );

  await clearSearch.click();
  await expect(search).toHaveValue("");
  await expect(searchSummary).toHaveText("1 activity · 1 process log visible");
  await expect(
    checkpoint.getByText("1 activity · 1 process log visible"),
  ).toBeVisible();
  await expect(clearSearch).toBeHidden();
  await expect(
    status.getByText("Synthetic output entered retry backoff"),
  ).toHaveCount(2);
  expect(await getCdpStatusTexts(page)).toContain(
    "1 activity · 1 process log visible",
  );
  expect(await getCdpNodeCount(page)).toBeLessThan(8_000);
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
  await expect(status.locator("#status-route-summary")).toHaveText(
    "Status loaded for seeded · commit seeded · 35 process logs · 5 notable activities",
  );
  await expect(checkpoint.getByText("35 process logs", { exact: true })).toBeVisible();
  await expect(
    checkpoint.getByText("5 notable activities", { exact: true }),
  ).toBeVisible();
  await expect(status.locator("#status-log-search-results-summary")).toHaveText(
    "5 activities · 35 process logs visible",
  );
  await expect(
    checkpoint.getByText("5 activities · 35 process logs visible"),
  ).toBeVisible();
  const logs = status.getByLabel("Process log entries");
  await expect(status.getByText("20 process logs shown of 35")).toBeVisible();
  await expect(logs.getByText("routine status log 20")).toBeVisible();
  await expect(logs.getByText("routine status log 21")).toHaveCount(0);
  const showAll = status.getByRole("button", { name: "Show all 35" });
  await expect(showAll).toHaveAttribute("aria-expanded", "false");
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
    checkpoint.getByText('0 activities · 1 process log match "log 35"'),
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
  const summary = incidents.locator("#incidents-route-summary");
  await expect(
    checkpoint.getByRole("heading", { name: "Incidents" }),
  ).toBeVisible();
  await expect(
    incidents
      .locator("#incidents-mode-content")
      .getByRole("heading", { name: "Incidents" }),
  ).toBeVisible();
  await expect(summary).toHaveText(
    "0 critical · 1 warning · 1 recent event · fleet",
  );
  await expect(
    checkpoint.getByText("0 critical · 1 warning", { exact: true }),
  ).toBeVisible();
  await expect(
    checkpoint.getByText("1 recent event", { exact: true }),
  ).toBeVisible();
  await expect(
    checkpoint.getByText("1 alert group · 1 event visible"),
  ).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    "0 critical · 1 warning · 1 recent event · fleet",
  );
  const search = incidents.getByLabel("Search incidents and events");
  const searchSummary = incidents.locator("#incidents-search-results-summary");
  await expect(searchSummary).toHaveText("1 alert group · 1 event visible");
  const retryingAlert = incidents
    .locator("[data-alert-id='seed-alert-retrying-output']")
    .first();
  await expect(
    retryingAlert.getByRole("heading", { name: "Retrying output" }),
  ).toBeVisible();
  await expect(
    retryingAlert.getByText("Recommended action:"),
  ).toHaveCount(0);
  await expect(retryingAlert.getByText("Evidence")).toHaveCount(0);
  await expect(
    retryingAlert.getByRole("button", { name: "Show alert details" }),
  ).toHaveAttribute("aria-expanded", "false");
  await retryingAlert
    .getByRole("button", { name: "Show alert details" })
    .click();
  await expect(
    retryingAlert.getByRole("button", { name: "Hide alert details" }),
  ).toHaveAttribute("aria-expanded", "true");
  await expect(retryingAlert.getByText("Recommended action:")).toBeVisible();
  await expect(retryingAlert.getByText("Evidence")).toBeVisible();
  await retryingAlert
    .getByRole("button", { name: "Hide alert details" })
    .click();
  await expect(
    retryingAlert.getByText("Recommended action:"),
  ).toHaveCount(0);

  await search.fill("destination");
  await expect(summary).toHaveText(
    "0 critical · 1 warning · 1 recent event · fleet",
  );
  await expect(searchSummary).toHaveText(
    '1 alert group · 1 event match "destination"',
  );
  await expect(
    checkpoint.getByText('1 alert group · 1 event match "destination"'),
  ).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    '1 alert group · 1 event match "destination"',
  );

  await search.fill("healthy");
  await expect(summary).toHaveText(
    "0 critical · 1 warning · 1 recent event · fleet",
  );
  await expect(searchSummary).toHaveText(
    '0 alert groups · 0 events match "healthy"',
  );
  await expect(
    checkpoint.getByText('0 alert groups · 0 events match "healthy"'),
  ).toBeVisible();
  await expect(
    incidents.getByText('No alert matches for "healthy".'),
  ).toBeVisible();
  const clearSearch = incidents.getByRole("button", { name: "Clear search" });
  await expect(clearSearch).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    '0 alert groups · 0 events match "healthy"',
  );

  await clearSearch.click();
  await expect(search).toHaveValue("");
  await expect(searchSummary).toHaveText("1 alert group · 1 event visible");
  await expect(
    checkpoint.getByText("1 alert group · 1 event visible"),
  ).toBeVisible();
  await expect(clearSearch).toBeHidden();
  await expect(
    incidents.getByRole("heading", { name: "Retrying output" }),
  ).toBeVisible();
  await expect(
    incidents.getByText('No alert matches for "healthy".'),
  ).toHaveCount(0);
  await expect(
    incidents.getByText('No event matches for "healthy".'),
  ).toHaveCount(0);
  expect(await getCdpStatusTexts(page)).toContain(
    "1 alert group · 1 event visible",
  );

  await incidents
    .getByLabel("Filter incidents by pipeline")
    .selectOption("pipe-healthy");
  await expect(summary).toHaveText(
    "0 critical · 0 warning · 0 recent events · Healthy Program",
  );
  await expect(
    checkpoint.getByText("0 critical · 0 warning", { exact: true }),
  ).toBeVisible();
  await expect(checkpoint.getByText("Healthy Program", { exact: true })).toBeVisible();
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
  await expect(checkpoint.getByText("3 critical", { exact: true })).toBeVisible();
  await expect(
    checkpoint.getByText("14 alert groups · 20 events visible"),
  ).toBeVisible();
  await expect(incidents.locator("#incidents-search-results-summary")).toHaveText(
    "14 alert groups · 20 events visible",
  );
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
  const showAllAlerts = incidents.getByRole("button", { name: "Show all 14" });
  const showAllEvents = eventList.getByRole("button", { name: "Show all 20" });
  await expect(showAllAlerts).toHaveAttribute("aria-expanded", "false");
  await expect(showAllEvents).toHaveAttribute("aria-expanded", "false");
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
  await expect(incidents.locator("#incidents-search-results-summary")).toHaveText(
    '1 alert group · 1 event match "dense 14"',
  );
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
  const summary = telemetry.locator("#telemetry-route-summary");
  await expect(
    checkpoint.getByRole("heading", { name: "Engineer telemetry" }),
  ).toBeVisible();
  await expect(
    telemetry
      .locator("#telemetry-mode-content")
      .getByRole("heading", { name: "Engineer telemetry" }),
  ).toBeVisible();
  await expect(summary).toHaveText(
    "Telemetry loaded · 2 ingests · 2 stages · 1 egress · 1 reader · Healthy Program",
  );
  await expect(checkpoint.getByText("Healthy Program", { exact: true })).toBeVisible();
  await expect(checkpoint.getByText("2 stage counters", { exact: true })).toBeVisible();
  await expect(checkpoint.getByText("1 egress", { exact: true })).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    "Telemetry loaded · 2 ingests · 2 stages · 1 egress · 1 reader · Healthy Program",
  );
  const hostSettings = telemetry.getByLabel("Host settings");
  await expect(
    hostSettings.getByText("1 host setting · health ready"),
  ).toBeVisible();
  await expect(
    hostSettings.getByRole("button", { name: "Show host settings" }),
  ).toHaveAttribute("aria-expanded", "false");
  await expect(hostSettings.getByText("Open file descriptors")).toHaveCount(0);
  await hostSettings.getByRole("button", { name: "Show host settings" }).click();
  await expect(
    hostSettings.getByRole("button", { name: "Hide host settings" }),
  ).toHaveAttribute("aria-expanded", "true");
  await expect(hostSettings.getByText("Open file descriptors")).toBeVisible();
  await hostSettings.getByRole("button", { name: "Hide host settings" }).click();
  await expect(hostSettings.getByText("Open file descriptors")).toHaveCount(0);

  await telemetry
    .getByLabel("Telemetry pipeline")
    .selectOption("pipe-retrying");
  await expect(summary).toHaveText(
    "Telemetry loaded · 2 ingests · 2 stages · 1 egress · 1 reader · Retrying Destination",
  );
  await expect(
    checkpoint.getByText("Retrying Destination", { exact: true }),
  ).toBeVisible();
  const search = telemetry.getByLabel("Search telemetry items");
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
    ),
  ).toBeVisible();
  await expect(telemetry.getByText('No readers match "video".')).toBeVisible();
  await expect(telemetry.getByText('No egresses match "video".')).toBeVisible();
  await expect(
    telemetry.getByRole("button", { name: "View video telemetry details" }),
  ).toBeVisible();
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
    ),
  ).toBeVisible();
  const clearSearch = telemetry.getByRole("button", { name: "Clear search" });
  await expect(clearSearch).toBeVisible();
  await expect(telemetry.getByText('No stages match "absent".')).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    '0/4 telemetry items match "absent" · 0 readers · 0 stages · 0 egresses',
  );

  await clearSearch.click();
  await expect(search).toHaveValue("");
  await expect(searchSummary).toHaveText(
    "1 reader · 2 stages · 1 egress visible",
  );
  await expect(
    checkpoint.getByText("1 reader · 2 stages · 1 egress visible"),
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
    telemetry.getByRole("button", { name: "Hide stage details" }),
  ).toBeVisible();
  await telemetry.getByRole("button", { name: "Hide stage details" }).click();
  await expect(telemetry.locator("#stage-telemetry-detail")).not.toContainText(
    "packetsOut",
  );
  await expect(
    telemetry.locator("#stage-telemetry-detail").getByText(
      "Select a stage to fetch its current detail.",
    ),
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
    .getByLabel("Telemetry pipeline")
    .selectOption("pipe-retrying");
  await expect(telemetry.locator("#telemetry-route-summary")).toHaveText(
    "Telemetry loaded · 2 ingests · 12 stages · 12 egresses · 1 reader · Retrying Destination",
  );
  await expect(checkpoint.getByText("12 egresses", { exact: true })).toBeVisible();
  await expect(
    checkpoint.getByText("1 reader · 12 stages · 12 egresses visible"),
  ).toBeVisible();
  await expect(telemetry.locator("#telemetry-search-results-summary")).toHaveText(
    "1 reader · 12 stages · 12 egresses visible",
  );

  const stages = telemetry.getByLabel("Telemetry processing stages");
  await expect(stages.getByText("8 stages shown of 12")).toBeVisible();
  await expect(stages.getByText("dense-stage-08")).toBeVisible();
  await expect(stages.getByText("dense-stage-09")).toHaveCount(0);
  const showAllStages = stages.getByRole("button", { name: "Show all 12" });
  await expect(showAllStages).toHaveAttribute("aria-expanded", "false");
  const egresses = telemetry.getByLabel("Telemetry egresses");
  await expect(egresses.getByText("8 egresses shown of 12")).toBeVisible();
  await expect(egresses.getByText("out-dense-08")).toBeVisible();
  await expect(egresses.getByText("out-dense-09")).toHaveCount(0);
  const showAll = egresses.getByRole("button", { name: "Show all 12" });
  await expect(showAll).toHaveAttribute("aria-expanded", "false");
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
  await expect(telemetry.locator("#telemetry-search-results-summary")).toHaveText(
    '1/25 telemetry items match "out-dense-12" · 0 readers · 0 stages · 1 egress',
  );
  await expect(
    checkpoint.getByText(
      '1/25 telemetry items match "out-dense-12" · 0 readers · 0 stages · 1 egress',
    ),
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
    page.locator("#dashboard-v2-pipeline-selector-root").getByRole("heading", {
      name: "Pipelines",
    }),
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
  const audioSearch = inputStatus.getByLabel("Search audio tracks");
  const audioSearchClear = inputStatus.getByRole("button", {
    name: "Clear search",
  });
  await expect(audioSearch).toBeVisible();
  await audioSearch.fill("track 30");
  await expect(inputStatus.getByText("Track 30")).toBeVisible();
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
  await audioSearchClear.click();
  await expect(audioSearch).toHaveValue("");
  await expect(inputStatus.getByText("Track 6")).toBeVisible();
  await expect(inputStatus.getByText("Track 30")).toHaveCount(0);
  await expect(audioSearchClear).toBeHidden();
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
  expect(
    await page.locator("#overview-mode-content").evaluate((node) => ({
      childCount: node.childElementCount,
      text: node.textContent?.trim() ?? "",
    })),
  ).toEqual({ childCount: 0, text: "" });

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
  expect(
    await page.locator("#pipelines").evaluate((node) => ({
      childCount: node.childElementCount,
      text: node.textContent?.trim() ?? "",
    })),
  ).toEqual({ childCount: 0, text: "" });
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
    await expect(
      outputOverview.getByRole("menu", {
        name: "More actions for Retrying Output",
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
        (
          window as Window & { __redesignOpenedUrls?: string[] }
        ).__redesignOpenedUrls,
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
    .getByRole("button", { name: "More actions for Retrying Output" })
    .click();
  await expect(
    outputOverview.getByRole("menu", {
      name: "More actions for Retrying Output",
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
  expect(await getCdpNodeCount(page)).toBeLessThan(4_800);
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

test("ui=v2 overview pipeline table supports large-fleet search @desktop", async ({
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
    const importModule = new Function(
      "path",
      "return import(path)",
    ) as (path: string) => Promise<{
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
            summary: "Main Program SRT ingest recovered without operator action.",
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
    overview.getByRole("button", { name: "Ritual backup" }),
  ).toBeVisible();
  await expect(
    overview.getByRole("button", { name: "Main Program" }),
  ).not.toBeVisible();
  const overviewClearSearch = overview.getByRole("button", {
    name: "Clear search",
  });
  await expect(overviewClearSearch).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    '1/10 pipelines shown · "ritual"',
  );

  await overviewClearSearch.click();
  await expect(overview.getByLabel("Search overview pipelines")).toHaveValue(
    "",
  );
  await expect(
    overview.getByRole("button", { name: "Main Program" }),
  ).toBeVisible();
  await expect(overviewClearSearch).toBeHidden();

  await overview.getByLabel("Search overview pipelines").fill("nowhere");
  await expect(overview.getByText("No pipelines match.")).toBeVisible();
  await expect(
    overview.getByText(
      'No overview pipelines match "nowhere". Clear search to show all.',
    ),
  ).toBeVisible();
  expect(await getCdpStatusTexts(page)).toEqual(
    expect.arrayContaining([
      '0/10 pipelines shown · "nowhere"',
      'No overview pipelines match "nowhere". Clear search to show all.',
    ]),
  );

  await overviewClearSearch.click();
  await expect(
    overview.getByRole("button", { name: "Main Program" }),
  ).toBeVisible();
  await expect(
    overview.getByRole("button", { name: "Ritual backup" }),
  ).toBeVisible();

  await expect(overview.getByLabel("Search restream activity")).toBeVisible();
  await overview.getByLabel("Search restream activity").fill("restored");
  await expect(overview.getByText("Input restored")).toBeVisible();
  await expect(overview.getByText("Output retry burst")).not.toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    '1/3 bursts shown · "restored"',
  );
  const clearActivitySearch = overview.getByRole("button", {
    name: "Clear activity search",
  });
  await expect(clearActivitySearch).toBeVisible();
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

  expect(await getCdpNodeCount(page)).toBeLessThan(1_500);
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
    <div id="dashboard-v2-pipeline-inspect-root"></div>
    <div id="dashboard-v2-control-room-root"></div>
    <div id="dashboard-v2-incidents-root"></div>
    <div id="dashboard-v2-telemetry-root"></div>
    <div id="dashboard-v2-status-root"></div>
    <div id="dashboard-v2-media-root"></div>
    <div id="dashboard-v2-settings-root"></div>
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

test("ui=v2 pipeline selector supports search under long lists @desktop", async ({
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
    const importModule = new Function(
      "path",
      "return import(path)",
    ) as (path: string) => Promise<{
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
    selector.getByRole("button", { name: /Ritual backup/ }),
  ).toBeVisible();
  await expect(
    selector.getByRole("button", { name: /Ritual backup/ }),
  ).toHaveAttribute("aria-current", "page");
  await expect(
    selector.getByRole("button", { name: /Main Program/ }),
  ).not.toBeVisible();
  const selectorClearSearch = selector.getByRole("button", {
    name: "Clear search",
  });
  await expect(selectorClearSearch).toBeVisible();
  expect(await getCdpStatusTexts(page)).toContain(
    '1/10 pipelines shown · "backup"',
  );

  await selectorClearSearch.click();
  await expect(selector.getByLabel("Search pipelines")).toHaveValue("");
  await expect(
    selector.getByRole("button", { name: /Main Program/ }),
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
    selector.getByRole("button", { name: /Main Program/ }),
  ).toBeVisible();
  await expect(
    selector.getByRole("button", { name: /Ritual backup/ }),
  ).toBeVisible();
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
    <div id="dashboard-v2-pipeline-inspect-root"></div>
    <div id="dashboard-v2-control-room-root"></div>
    <div id="dashboard-v2-incidents-root"></div>
    <div id="dashboard-v2-telemetry-root"></div>
    <div id="dashboard-v2-status-root"></div>
    <div id="dashboard-v2-media-root"></div>
    <div id="dashboard-v2-settings-root"></div>
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

test("ui=v2 output action menus are keyboard-dismissable @desktop", async ({
  page,
}) => {
  await openSeededDashboard(
    page,
    "mixed-health",
    "/?mode=pipeline&view=operate&p=pipe-healthy&ui=v2",
    { expectOverviewReady: false },
  );

  const outputOverview = page.locator(
    "#dashboard-v2-pipeline-output-overview-root",
  );
  const more = outputOverview.getByRole("button", {
    name: "More actions for Healthy Output",
  });
  await more.focus();
  await page.keyboard.press("Enter");
  const menu = outputOverview.getByRole("menu", {
    name: "More actions for Healthy Output",
  });
  await expect(menu).toBeVisible();
  await expect(more).toHaveAttribute("aria-expanded", "true");

  const cdp = await page.context().newCDPSession(page);
  const axTree = await cdp.send("Accessibility.getFullAXTree");
  const menuRoles = axTree.nodes
    .filter((node) => node.name?.value === "More actions for Healthy Output")
    .map((node) => node.role?.value);
  expect(menuRoles).toContain("menu");
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
    <div id="dashboard-v2-pipeline-inspect-root"></div>
    <div id="dashboard-v2-control-room-root"></div>
    <div id="dashboard-v2-incidents-root"></div>
    <div id="dashboard-v2-telemetry-root"></div>
    <div id="dashboard-v2-status-root"></div>
    <div id="dashboard-v2-media-root"></div>
    <div id="dashboard-v2-settings-root"></div>
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
  const clearActiveFilters = root.getByRole("button", {
    name: "Clear output filters",
  });
  await expect(clearActiveFilters).toBeVisible();
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
  await root.getByRole("button", { name: "Stopped" }).click();
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
  await cdp.detach();

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
