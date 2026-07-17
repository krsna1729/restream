import { expect, type Locator, type Page } from "@playwright/test";

export async function getCdpStatusTexts(page: Page): Promise<string[]> {
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

export async function getCdpNamesByRole(
  page: Page,
  role: string,
): Promise<string[]> {
  const cdp = await page.context().newCDPSession(page);
  const axTree = await cdp.send("Accessibility.getFullAXTree");
  await cdp.detach();
  return axTree.nodes
    .filter((node) => node.role?.value === role)
    .map((node) => node.name?.value)
    .filter((name): name is string => Boolean(name));
}

export async function getCdpNodeCount(page: Page): Promise<number> {
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Performance.enable");
  const performanceMetrics = await cdp.send("Performance.getMetrics");
  await cdp.detach();
  return (
    performanceMetrics.metrics.find((metric) => metric.name === "Nodes")
      ?.value ?? 0
  );
}

export async function getCdpLayoutWidthDelta(page: Page): Promise<number> {
  const cdp = await page.context().newCDPSession(page);
  const metrics = await cdp.send("Page.getLayoutMetrics");
  await cdp.detach();
  return metrics.contentSize.width - metrics.cssLayoutViewport.clientWidth;
}

export async function getDocumentWidthOverflow(page: Page): Promise<number> {
  return page.evaluate(
    () =>
      document.documentElement.scrollWidth -
      document.documentElement.clientWidth,
  );
}

export async function selectPipelineInV2Selector(
  root: Locator,
  pipelineId: string,
  pipelineName: string | RegExp,
): Promise<void> {
  await expect(root).toBeVisible();
  const compactSelector = root.getByLabel("Select pipeline");
  const pipelineButton = root.getByRole("button", { name: pipelineName });
  await expect
    .poll(
      async () =>
        (await compactSelector.count()) + (await pipelineButton.count()),
    )
    .toBeGreaterThan(0);
  if ((await compactSelector.count()) > 0) {
    await expect(compactSelector).toBeVisible();
    await compactSelector.selectOption(pipelineId);
    return;
  }
  await pipelineButton.click();
}

export async function expectTabVisibleInRail(
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

export async function installPushStateCounter(page: Page): Promise<void> {
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

export async function resetPushStateCounter(page: Page): Promise<void> {
  await page.evaluate(() => {
    (
      window as Window & { __redesignPushStateCount?: number }
    ).__redesignPushStateCount = 0;
  });
}

export async function expectPushStateCount(
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

export async function tabUntilFocused(
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
