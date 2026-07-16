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

  expect(await getCdpNamesByRole(page, "heading")).toEqual(
    expect.arrayContaining([
      "PIPELINES",
      "Recovered Sink Flap",
      "INPUT AND PREVIEW",
      "OUTPUT OVERVIEW",
    ]),
  );
  expect(await getCdpNamesByRole(page, "button")).toEqual(
    expect.arrayContaining(["Graph", "Diagnose", "Show all 30"]),
  );
});
