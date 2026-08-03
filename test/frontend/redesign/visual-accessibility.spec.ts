import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

import { openSeededDashboard, type SeededDashboardOptions } from "./fixtures";
import { type OperatorStateName } from "./fixtures/operator-states";

const FIXED_TIME = new Date("2026-07-14T06:30:10Z");
const ADD_PIPELINE_SELECTOR = '[aria-label="Add a new pipeline"]';
const viewportStates: OperatorStateName[] = ["empty", "mixed-health"];

async function openStableOverview(
  page: Page,
  stateName: OperatorStateName,
  options: SeededDashboardOptions = {},
): Promise<void> {
  await page.clock.setFixedTime(FIXED_TIME);
  await page.emulateMedia({ reducedMotion: "reduce" });
  await openSeededDashboard(page, stateName, "/?mode=overview", options);
  await expect(page.locator("#dashboard-v2-overview")).toBeVisible();
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
  let axTree;
  try {
    axTree = await cdp.send("Accessibility.getFullAXTree");
  } finally {
    await cdp.detach();
  }
  return axTree.nodes
    .filter((node) => node.role?.value === role)
    .map((node) => node.name?.value)
    .filter((name): name is string => Boolean(name));
}

// axTree.nodes is a flat array in internal computation order, which does not
// reliably match document/reading order across independently-mounted roots
// (verified: the pipeline header root renders before the output-overview
// root in the DOM, but flat array order put them the other way round). Walk
// the tree via parentId/childIds instead so heading order reflects true
// reading order, while still using CDP's computed accessible name (which,
// unlike raw textContent, reflects CSS text-transform).
async function getCdpHeadingLevels(
  page: Page,
): Promise<{ name: string; level: number }[]> {
  const cdp = await page.context().newCDPSession(page);
  let axTree;
  try {
    axTree = await cdp.send("Accessibility.getFullAXTree");
  } finally {
    await cdp.detach();
  }
  const byId = new Map(axTree.nodes.map((node) => [node.nodeId, node]));
  const root = axTree.nodes.find((node) => !node.parentId) ?? axTree.nodes[0];
  const headings: { name: string; level: number }[] = [];
  const visit = (nodeId: string) => {
    const node = byId.get(nodeId);
    if (!node) return;
    if (node.role?.value === "heading" && node.name?.value) {
      const level =
        node.properties?.find((property) => property.name === "level")?.value
          ?.value ?? 0;
      headings.push({
        level: typeof level === "number" ? level : Number(level),
        name: node.name.value,
      });
    }
    for (const childId of node.childIds ?? []) visit(childId);
  };
  visit(root.nodeId);
  return headings;
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
    await expect(page.locator(ADD_PIPELINE_SELECTOR)).toBeVisible();
    if (stateName === "mixed-health") {
      const attentionBox = await page
        .getByRole("region", { name: "1 pipeline needs attention" })
        .boundingBox();
      const signalsBox = await page
        .getByRole("region", { name: "Fleet signals" })
        .boundingBox();
      const pipelineBox = await page
        .getByRole("region", { name: "All pipelines" })
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
    await reachFromOverviewTab(page, ADD_PIPELINE_SELECTOR);
    await expect(page.locator(ADD_PIPELINE_SELECTOR)).toBeFocused();
    await page.waitForTimeout(6_000);
    await expect(page.locator(ADD_PIPELINE_SELECTOR)).toBeFocused();
    await page.locator(ADD_PIPELINE_SELECTOR).press("Enter");

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

test("axe/cdp: default dashboard Operate preserves contrast and semantic landmarks", async ({
  page,
}) => {
  await page.clock.setFixedTime(FIXED_TIME);
  await page.emulateMedia({ reducedMotion: "reduce" });
  await openSeededDashboard(
    page,
    "chaos-recovery",
    "/?mode=pipeline&view=operate&p=pipe-flapping",
    { expectOverviewReady: false },
  );

  await expect(
    page.locator("#dashboard-v2-pipeline-header-root").getByRole("heading", {
      name: "Recovered Sink Flap",
    }),
  ).toBeVisible();
  const results = await new AxeBuilder({ page })
    .include("#dashboard-v2-operate-panel")
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

test("cdp: default dashboard route heading outlines stay operator-clean @desktop", async ({
  page,
}) => {
  await page.clock.setFixedTime(FIXED_TIME);
  await page.emulateMedia({ reducedMotion: "reduce" });
  const routes = [
    {
      href: "/?mode=overview",
      readySelector: "#dashboard-v2-overview",
      topHeading: "Fleet overview",
    },
    {
      href: "/?mode=pipeline&view=operate&p=pipe-retrying",
      readySelector: "#dashboard-v2-pipeline-header-root",
      topHeading: "Retrying Destination",
    },
    {
      href: "/?mode=pipeline&view=inspect&p=pipe-retrying",
      checkpointActionRoot: "#dashboard-v2-pipeline-inspect-root",
      checkpointHeading: "Retrying Destination",
      readySelector: "#dashboard-v2-pipeline-inspect-root",
      topHeading: "Pipeline inspect",
    },
    {
      href: "/?mode=pipeline&view=monitor&p=pipe-retrying",
      checkpointActionRoot: "#dashboard-v2-control-room-root",
      checkpointHeading: "Retrying Destination",
      readySelector: "#dashboard-v2-control-room-root",
      topHeading: "Control Room",
    },
    {
      href: "/?mode=media",
      checkpointActionRoot: "#dashboard-v2-media-root",
      checkpointHeading: "Media",
      readySelector: "#media-library-results-summary",
      topHeading: "Media Library",
    },
    {
      href: "/?mode=settings",
      checkpointActionRoot: "#dashboard-v2-settings-root",
      checkpointHeading: "Settings",
      readySelector: "#dashboard-v2-settings-root",
      topHeading: "Settings",
    },
    {
      href: "/?mode=status",
      checkpointActionRoot: "#dashboard-v2-status-root",
      checkpointHeading: "Status",
      readySelector: "#dashboard-v2-status-root",
      topHeading: "Status",
    },
    {
      href: "/?mode=incidents",
      checkpointActionRoot: "#dashboard-v2-incidents-root",
      checkpointHeading: "Incidents",
      readySelector: "#dashboard-v2-incidents-root",
      topHeading: "Incidents",
    },
    {
      href: "/?mode=telemetry",
      checkpointActionRoot: "#dashboard-v2-telemetry-root",
      checkpointHeading: "Engineer telemetry",
      readySelector: "#dashboard-v2-telemetry-root",
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
    if ("checkpointActionRoot" in route) {
      await page
        .locator(route.checkpointActionRoot)
        .waitFor({ state: "visible" });
    }
    const headings = await getCdpHeadingLevels(page);
    expect(headings, route.href).not.toEqual([]);
    const expectedFirstHeadings =
      "checkpointHeading" in route
        ? [route.topHeading, route.checkpointHeading]
        : [route.topHeading];
    expect(headings[0], route.href).toEqual({
      level: 1,
      name: expect.stringMatching(
        new RegExp(
          `^(${expectedFirstHeadings
            .map((heading) => heading.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"))
            .join("|")})$`,
        ),
      ),
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
      expect(headings, route.href).toEqual(
        expect.arrayContaining([
          { level: 1, name: route.checkpointHeading },
        ]),
      );
      const actionButtons = await page
        // .btn-xs is daisyUI's deliberately compact button size, used
        // throughout for dense per-row table actions (media library
        // Rename/Delete/Play/Download); it is not meant to satisfy a
        // 44px touch target and is excluded here for that reason.
        .locator(`${route.checkpointActionRoot} button:not(.btn-xs)`)
        .evaluateAll((buttons) =>
          buttons
            .filter((button) => {
              if (button.closest("[hidden], [aria-hidden='true']")) return false;
              if ("checkVisibility" in button) {
                return button.checkVisibility({ checkVisibilityCSS: true });
              }
              const rect = button.getBoundingClientRect();
              return rect.width > 0 && rect.height > 0;
            })
            .map((button) => {
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

test("cdp: default dashboard wayfinding and next-step controls keep sturdy targets @desktop", async ({
  page,
}) => {
  await page.clock.setFixedTime(FIXED_TIME);
  await page.emulateMedia({ reducedMotion: "reduce" });
  const routes = [
    {
      href: "/?mode=overview",
      readySelector: "#dashboard-v2-overview",
    },
    {
      href: "/?mode=pipeline&view=operate&p=pipe-retrying",
      readySelector: "#dashboard-v2-pipeline-header-root",
    },
    {
      href: "/?mode=pipeline&view=inspect&p=pipe-retrying",
      readySelector: "#dashboard-v2-pipeline-inspect-root",
    },
    {
      href: "/?mode=pipeline&view=monitor&p=pipe-retrying",
      readySelector: "#dashboard-v2-control-room-root",
    },
    {
      href: "/?mode=media",
      readySelector: "#media-library-results-summary",
    },
    {
      href: "/?mode=settings",
      readySelector: "#dashboard-v2-settings-root",
    },
    {
      href: "/?mode=status",
      readySelector: "#dashboard-v2-status-root",
    },
    {
      href: "/?mode=incidents",
      readySelector: "#dashboard-v2-incidents-root",
    },
    {
      href: "/?mode=telemetry",
      readySelector: "#dashboard-v2-telemetry-root",
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
    const undersizedControls = await page.evaluate(() => {
      const selector = [
        "#skip-to-dashboard-main",
        "#workspace-mode-bar [role='tab']",
        "#pipeline-workspace-view-bar:not(.hidden) [role='tab']",
        "button[aria-label^='Add a new pipeline']",
        "button[aria-label^='Open restream']",
        "button[aria-label^='Operate ']",
        "button[aria-label^='Inspect ']",
        "button[aria-label^='Diagnose ']",
        "button[aria-label^='Monitor ']",
      ].join(",");
      return Array.from(document.querySelectorAll<HTMLElement>(selector))
        .filter((element) => {
          if (element.closest("[hidden], [aria-hidden='true']")) return false;
          const style = window.getComputedStyle(element);
          if (
            style.display === "none" ||
            style.visibility === "hidden" ||
            style.pointerEvents === "none"
          )
            return false;
          const rect = element.getBoundingClientRect();
          return rect.width > 0 && rect.height > 0;
        })
        .map((element) => {
          const rect = element.getBoundingClientRect();
          return {
            height: Math.round(rect.height),
            label:
              element.getAttribute("aria-label") ||
              element.textContent?.replace(/\s+/g, " ").trim().slice(0, 80) ||
              element.getAttribute("placeholder") ||
              element.id ||
              element.tagName.toLowerCase(),
            tag: element.tagName.toLowerCase(),
            width: Math.round(rect.width),
          };
        })
        .filter(
          (control) =>
            control.height < 36 ||
            control.width < 44,
        );
    });
    expect(undersizedControls, route.href).toEqual([]);
  }
});

test("axe/cdp: default dashboard routes expose named controls without serious accessibility findings @desktop", async ({
  page,
}) => {
  await page.clock.setFixedTime(FIXED_TIME);
  await page.emulateMedia({ reducedMotion: "reduce" });
  const routes = [
    {
      href: "/?mode=overview",
      maxVisibleDashboardElements: 220,
      maxVisibleControls: 22,
      maxVisibleTextChars: 1500,
      readySelector: "#dashboard-v2-overview",
    },
    {
      href: "/?mode=pipeline&view=operate&p=pipe-retrying",
      maxVisibleDashboardElements: 180,
      maxVisibleControls: 28,
      maxVisibleTextChars: 1400,
      readySelector: "#dashboard-v2-pipeline-header-root",
    },
    {
      href: "/?mode=pipeline&view=inspect&p=pipe-retrying",
      maxVisibleDashboardElements: 180,
      maxVisibleControls: 22,
      maxVisibleTextChars: 1700,
      readySelector: "#dashboard-v2-pipeline-inspect-root",
    },
    {
      href: "/?mode=pipeline&view=monitor&p=pipe-retrying",
      maxVisibleDashboardElements: 130,
      maxVisibleControls: 30,
      maxVisibleTextChars: 1400,
      readySelector: "#dashboard-v2-control-room-root",
    },
    {
      href: "/?mode=media",
      maxVisibleDashboardElements: 100,
      maxVisibleControls: 18,
      maxVisibleTextChars: 1000,
      readySelector: "#media-library-results-summary",
    },
    {
      href: "/?mode=settings",
      maxVisibleDashboardElements: 140,
      maxVisibleControls: 26,
      maxVisibleTextChars: 1600,
      readySelector: "#dashboard-v2-settings-root",
    },
    {
      href: "/?mode=status",
      maxVisibleDashboardElements: 130,
      maxVisibleControls: 22,
      maxVisibleTextChars: 1600,
      readySelector: "#dashboard-v2-status-root",
    },
    {
      href: "/?mode=incidents",
      maxVisibleDashboardElements: 110,
      maxVisibleControls: 20,
      maxVisibleTextChars: 1300,
      readySelector: "#dashboard-v2-incidents-root",
    },
    {
      href: "/?mode=telemetry",
      maxVisibleDashboardElements: 160,
      maxVisibleControls: 18,
      maxVisibleTextChars: 1600,
      readySelector: "#dashboard-v2-telemetry-root",
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

    const results = await new AxeBuilder({ page })
      .include("#dashboard-main")
      .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"])
      .analyze();
    const blocking = results.violations.filter(
      (violation) =>
        violation.impact === "serious" || violation.impact === "critical",
    );
    expect(blocking, route.href).toEqual([]);

    const controls = await page.evaluate(() => {
      const selector = [
        "button",
        "a[href]",
        "input",
        "select",
        "summary",
        '[role="button"]',
        '[role="tab"]',
        '[role="menuitem"]',
      ].join(",");
      return Array.from(document.querySelectorAll<HTMLElement>(selector))
        .filter((element) => {
          if (element.closest("[hidden], [aria-hidden='true']")) return false;
          const visible =
            "checkVisibility" in element
              ? element.checkVisibility({ checkVisibilityCSS: true })
              : true;
          if (!visible) return false;
          const style = window.getComputedStyle(element);
          if (
            style.display === "none" ||
            style.visibility === "hidden" ||
            style.pointerEvents === "none"
          )
            return false;
          const rect = element.getBoundingClientRect();
          return rect.width > 0 && rect.height > 0;
        })
        .map((element) => {
          const formLabels =
            element instanceof HTMLInputElement ||
            element instanceof HTMLSelectElement ||
            element instanceof HTMLTextAreaElement
              ? Array.from(element.labels ?? [])
                  .map((label) => label.textContent?.replace(/\s+/g, " ").trim())
                  .filter(Boolean)
                  .join(" ")
              : "";
          const label =
            element.getAttribute("aria-label") ||
            element.getAttribute("aria-labelledby") ||
            element.getAttribute("title") ||
            element.textContent?.replace(/\s+/g, " ").trim() ||
            formLabels ||
            element.getAttribute("placeholder") ||
            "";
          return {
            id: element.id || null,
            label,
            tag: element.tagName.toLowerCase(),
            type: element.getAttribute("type"),
          };
        });
    });
    expect(
      controls.length,
      `${route.href} visible controls: ${controls
        .map((control) => control.label || control.id || control.tag)
        .join(", ")}`,
    ).toBeLessThanOrEqual(route.maxVisibleControls);
    const unnamedControls = controls.filter((control) => !control.label);
    expect(unnamedControls, route.href).toEqual([]);
    const genericActionControls = controls.filter((control) =>
      /^(More|Show|Hide) actions for\b/.test(control.label),
    );
    expect(
      genericActionControls,
      `${route.href} controls should name their action domain`,
    ).toEqual([]);
    const visibleDashboardElements = await page.evaluate(() =>
      Array.from(
        document.querySelectorAll<HTMLElement>("#dashboard-main *"),
      ).filter((element) => {
        if (element.closest("[hidden], [aria-hidden='true']")) return false;
        const visible =
          "checkVisibility" in element
            ? element.checkVisibility({ checkVisibilityCSS: true })
            : true;
        if (!visible) return false;
        const style = window.getComputedStyle(element);
        if (
          style.display === "none" ||
          style.visibility === "hidden" ||
          style.pointerEvents === "none"
        )
          return false;
        const rect = element.getBoundingClientRect();
        return rect.width > 0 && rect.height > 0;
      }).length,
    );
    expect(
      visibleDashboardElements,
      `${route.href} visible dashboard elements`,
    ).toBeLessThanOrEqual(route.maxVisibleDashboardElements);
    const visibleTextChars = await page.evaluate(
      () =>
        document.querySelector<HTMLElement>("#dashboard-main")?.innerText
          .length ?? 0,
    );
    expect(
      visibleTextChars,
      `${route.href} visible operator text characters`,
    ).toBeLessThanOrEqual(route.maxVisibleTextChars);
  }
});
