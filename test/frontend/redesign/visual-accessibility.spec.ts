import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

import { openSeededDashboard, type SeededDashboardOptions } from "./fixtures";
import { type OperatorStateName } from "./fixtures/operator-states";

const FIXED_TIME = new Date("2026-07-14T06:30:10Z");
const viewportStates: OperatorStateName[] = ["empty", "mixed-health"];

async function openStableOverview(
  page: Page,
  stateName: OperatorStateName,
  options: SeededDashboardOptions = {},
): Promise<void> {
  await page.clock.setFixedTime(FIXED_TIME);
  await page.emulateMedia({ reducedMotion: "reduce" });
  await openSeededDashboard(page, stateName, "/?mode=overview", options);
  await expect(page.locator("#overview-mode-content")).toBeVisible();
}

async function reachFromOverviewTab(
  page: Page,
  selector: string,
): Promise<void> {
  await page.locator("#workspace-tab-overview").focus();
  const focusPath: string[] = [];
  for (let attempt = 0; attempt < 20; attempt += 1) {
    await page.keyboard.press("Tab");
    const focused = await page.evaluate(() => {
      const element = document.activeElement as HTMLElement | null;
      return (
        element?.id || element?.textContent?.trim().slice(0, 40) || "unknown"
      );
    });
    focusPath.push(focused);
    if (
      await page
        .locator(selector)
        .evaluate((element) => element === document.activeElement)
    ) {
      return;
    }
  }
  throw new Error(
    `Keyboard focus did not reach ${selector}: ${focusPath.join(" -> ")}`,
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

async function getCdpHeadingLevels(
  page: Page,
): Promise<{ name: string; level: number }[]> {
  const cdp = await page.context().newCDPSession(page);
  const axTree = await cdp.send("Accessibility.getFullAXTree");
  await cdp.detach();
  return axTree.nodes
    .filter((node) => node.role?.value === "heading")
    .map((node) => {
      const level =
        node.properties?.find((property) => property.name === "level")?.value
          ?.value ?? 0;
      return {
        level: typeof level === "number" ? level : Number(level),
        name: node.name?.value ?? "",
      };
    })
    .filter(({ name }) => Boolean(name));
}

for (const stateName of viewportStates) {
  test(`visual: ${stateName} Overview matches the pinned viewport`, async ({
    page,
  }) => {
    await openStableOverview(page, stateName);

    const pageOverflow = await page.evaluate(
      () =>
        document.documentElement.scrollWidth -
        document.documentElement.clientWidth,
    );
    expect(pageOverflow).toBeLessThanOrEqual(1);
    await expect(page).toHaveScreenshot(`overview-${stateName}.png`, {
      animations: "disabled",
      caret: "hide",
      fullPage: true,
    });
    await expect(page.locator("#workspace-tab-overview")).toBeVisible();
    await expect(page.locator("#overview-add-pipeline-btn")).toBeVisible();
    if (stateName === "mixed-health") {
      const attentionBox = await page
        .locator("#overview-attention")
        .boundingBox();
      const signalsBox = await page
        .locator("#overview-fleet-signals")
        .boundingBox();
      const pipelineBox = await page
        .locator("#overview-pipelines")
        .boundingBox();
      expect(attentionBox).not.toBeNull();
      expect(signalsBox).not.toBeNull();
      expect(pipelineBox).not.toBeNull();
      expect(attentionBox!.y).toBeLessThan(pipelineBox!.y);
      if (test.info().project.name === "mobile-390x844") {
        expect(attentionBox!.y).toBeLessThan(signalsBox!.y);
        const issueBox = await page
          .getByRole("heading", { name: "Retrying Destination" })
          .boundingBox();
        expect(issueBox).not.toBeNull();
        expect(issueBox!.y + issueBox!.height).toBeLessThanOrEqual(844);
      }
    }
  });
}

test.describe("desktop accessibility contract @desktop", () => {
  test("keyboard: periodic refresh retains focus through the Add Pipeline tab path", async ({
    page,
  }) => {
    await openStableOverview(page, "empty", {
      runtimeResponse: (runtime, requestCount) => {
        const metrics = runtime.metrics as Record<string, unknown>;
        const engine = metrics.engine as Record<string, unknown>;
        return {
          ...runtime,
          metrics: {
            ...metrics,
            engine: { ...engine, cpuPercent: 7 + requestCount },
          },
        };
      },
    });
    await reachFromOverviewTab(page, "#overview-add-pipeline-btn");
    await expect(page.locator("#overview-add-pipeline-btn")).toBeFocused();
    await page.waitForTimeout(6_000);
    await expect(page.locator("#overview-add-pipeline-btn")).toBeFocused();
    await page.locator("#overview-add-pipeline-btn").press("Enter");

    const editor = page.locator("#edit-pipe-modal");
    await expect(editor).toBeVisible();
    await expect(
      editor.getByRole("heading", { name: "Add Pipeline" }),
    ).toBeVisible();
    await expect(page.locator("#pipe-name-input")).toBeFocused();
    await page.keyboard.press("Escape");
    await expect(editor).toBeHidden();
  });

  test("ARIA: mixed-health Overview retains its operator structure", async ({
    page,
  }) => {
    await openStableOverview(page, "mixed-health");
    await expect(page.locator("#overview-mode-panel")).toMatchAriaSnapshot({
      name: "overview-mixed-health.aria.yml",
    });
  });

  for (const stateName of viewportStates) {
    test(`axe: ${stateName} Overview has no serious or critical findings`, async ({
      page,
    }) => {
      await openStableOverview(page, stateName);
      const results = await new AxeBuilder({ page })
        .include("#overview-mode-panel")
        .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"])
        .analyze();
      const blocking = results.violations.filter(
        (violation) =>
          violation.impact === "serious" || violation.impact === "critical",
      );
      expect(blocking).toEqual([]);
    });
  }
});

test("axe/cdp: ui=v2 Operate preserves contrast and semantic landmarks", async ({
  page,
}) => {
  await page.clock.setFixedTime(FIXED_TIME);
  await page.emulateMedia({ reducedMotion: "reduce" });
  await openSeededDashboard(
    page,
    "chaos-recovery",
    "/?mode=pipeline&view=operate&p=pipe-flapping&ui=v2",
    { expectOverviewReady: false },
  );

  await expect(
    page.locator("#dashboard-v2-pipeline-header-root").getByRole("heading", {
      name: "Recovered Sink Flap",
    }),
  ).toBeVisible();
  const results = await new AxeBuilder({ page })
    .include("#dashboard-grid")
    .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"])
    .analyze();
  const blocking = results.violations.filter(
    (violation) =>
      violation.impact === "serious" || violation.impact === "critical",
  );
  expect(blocking).toEqual([]);

  const headingNames = await getCdpNamesByRole(page, "heading");
  expect(headingNames).toEqual(
    expect.arrayContaining([
      "Recovered Sink Flap",
      "INPUT AND PREVIEW",
      "OUTPUT OVERVIEW",
    ]),
  );
  expect(headingNames).not.toContain("PIPELINES");
  expect(await getCdpNamesByRole(page, "button")).toEqual(
    expect.arrayContaining([
      "Inspect graph for Recovered Sink Flap",
      "Diagnose Recovered Sink Flap",
      "Show all 30 audio tracks",
    ]),
  );
  expect(await getCdpHeadingLevels(page)).toEqual(
    expect.arrayContaining([
      { name: "Recovered Sink Flap", level: 1 },
      { name: "INPUT AND PREVIEW", level: 2 },
      { name: "OUTPUT OVERVIEW", level: 2 },
      { name: "PREVIEW PLAYER", level: 3 },
      { name: "AUDIO", level: 3 },
      { name: "OUTPUT DESTINATIONS", level: 3 },
    ]),
  );
});

test("cdp: ui=v2 route heading outlines stay operator-clean @desktop", async ({
  page,
}) => {
  await page.clock.setFixedTime(FIXED_TIME);
  await page.emulateMedia({ reducedMotion: "reduce" });
  const routes = [
    {
      href: "/?mode=overview&ui=v2",
      readySelector: "#dashboard-v2-overview",
      topHeading: "Fleet overview",
    },
    {
      href: "/?mode=pipeline&view=operate&p=pipe-retrying&ui=v2",
      readySelector: "#dashboard-v2-pipeline-header-root",
      topHeading: "Retrying Destination",
    },
    {
      href: "/?mode=pipeline&view=inspect&p=pipe-retrying&ui=v2",
      checkpointActionRoot: "#dashboard-v2-pipeline-inspect-root",
      readySelector: "#inspect-route-summary",
      topHeading: "Pipeline inspect",
    },
    {
      href: "/?mode=pipeline&view=monitor&p=pipe-retrying&ui=v2",
      checkpointActionRoot: "#dashboard-v2-control-room-root",
      readySelector: "#control-room-route-summary",
      topHeading: "Control Room",
    },
    {
      href: "/?mode=media&ui=v2",
      checkpointActionRoot: "#dashboard-v2-media-root",
      readySelector: "#media-library-results-summary",
      topHeading: "Media Library",
    },
    {
      href: "/?mode=settings&ui=v2",
      checkpointActionRoot: "#dashboard-v2-settings-root",
      readySelector: "#settings-route-summary",
      topHeading: "Settings",
    },
    {
      href: "/?mode=status&ui=v2",
      checkpointActionRoot: "#dashboard-v2-status-root",
      readySelector: "#status-route-summary",
      topHeading: "Status",
    },
    {
      href: "/?mode=incidents&ui=v2",
      checkpointActionRoot: "#dashboard-v2-incidents-root",
      readySelector: "#incidents-route-summary",
      topHeading: "Incidents",
    },
    {
      href: "/?mode=telemetry&ui=v2",
      checkpointActionRoot: "#dashboard-v2-telemetry-root",
      readySelector: "#telemetry-route-summary",
      topHeading: "Engineer telemetry",
    },
  ] as const;

  await openSeededDashboard(page, "mixed-health", routes[0].href, {
    expectOverviewReady: false,
  });

  for (const route of routes) {
    if (page.url() !== new URL(route.href, page.url()).href) {
      await page.goto(route.href);
    }
    await page.locator(route.readySelector).waitFor({ state: "visible" });
    const headings = await getCdpHeadingLevels(page);
    expect(headings, route.href).not.toEqual([]);
    expect(headings[0], route.href).toEqual({
      level: 1,
      name: route.topHeading,
    });
    const duplicateHeadingNames = headings
      .map((heading) => heading.name)
      .filter((name, index, names) => names.indexOf(name) !== index);
    expect(duplicateHeadingNames, route.href).toEqual([]);
    for (let index = 1; index < headings.length; index += 1) {
      expect(headings[index].level, route.href).toBeLessThanOrEqual(
        headings[index - 1].level + 1,
      );
    }
    const pageOverflow = await page.evaluate(
      () =>
        document.documentElement.scrollWidth -
        document.documentElement.clientWidth,
    );
    expect(pageOverflow, route.href).toBeLessThanOrEqual(1);
    if ("checkpointActionRoot" in route) {
      await page
        .locator(route.checkpointActionRoot)
        .waitFor({ state: "visible" });
      const actionButtons = await page
        .locator(`${route.checkpointActionRoot} button`)
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
      expect(actionButtons, route.href).not.toEqual([]);
      for (const button of actionButtons) {
        expect(
          button.height,
          `${route.href} ${button.label}`,
        ).toBeGreaterThanOrEqual(36);
        expect(
          button.width,
          `${route.href} ${button.label}`,
        ).toBeGreaterThanOrEqual(44);
      }
    }
  }
});
