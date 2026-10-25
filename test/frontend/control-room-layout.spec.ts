import { expect, test } from "@playwright/test";

import { auditLayout } from "../../scripts/dev/frontend/layout-audit.mjs";

// Layout regressions on every workspace, at a desktop and a phone width:
// no sideways page scroll, no stray scrollbars, no select text under its
// arrow, controls on one row at one height and centre, touch-sized targets
// on the phone, text at WCAG AA contrast against what is behind it, and the
// pipeline tabs on every pipeline view (Logs too).
// The same audit drives the screenshot matrix (scripts/dev/frontend/shots.mjs).

const WORKSPACES = [
  "fleet",
  "channels",
  "pipelines",
  "operate",
  "wall",
  "health",
  "monitor",
  "inspect",
  "logs",
  "schedule",
  "incidents",
  "capacity",
  "stability",
  "media",
  "changes",
  "team",
  "audit",
  "notifications",
  "settings",
  "system",
  "guides",
  "architecture",
];
const PIPELINE_VIEWS = new Set(["operate", "health", "monitor", "inspect", "logs"]);

for (const [width, scheme] of [[1440, "dark"], [1440, "light"], [375, "dark"], [375, "light"]] as const) {
  test(`layout: every workspace at ${width} px, ${scheme}`, async ({ browser }) => {
    test.setTimeout(180_000);
    const phone = width < 768;
    // The theme follows the system unless chosen: the colour scheme picks it.
    const context = await browser.newContext({ viewport: { width, height: 900 }, hasTouch: phone, isMobile: phone, colorScheme: scheme });
    const page = await context.newPage();
    const login = await page.request.post("/api/v1/auth/login", { data: { password: "admin" } });
    expect(login.ok()).toBe(true);
    const stamp = Date.now();
    const created = await page.request.post("/api/v1/pipelines", { data: { name: `cr-layout-${width}-${scheme}-${stamp}`, streamKey: `crlayout${width}${scheme}_${stamp}` } });
    expect(created.ok()).toBe(true);
    const pipelineId: string = (await created.json()).pipeline.id;
    try {
      const output = await page.request.post(`/api/v1/pipelines/${pipelineId}/outputs`, {
        data: { name: "cr-layout-out", url: "rtmp://127.0.0.1:9/live/layout", config: { video: { mode: "source" }, audio: { mode: "all" } } },
      });
      expect(output.ok()).toBe(true);
      const problems: string[] = [];
      for (const workspace of WORKSPACES) {
        await page.goto(`/?w=${workspace}&p=${pipelineId}`);
        await expect(page.locator("main h1, main h2").first()).toBeVisible();
        if (PIPELINE_VIEWS.has(workspace)) {
          await expect(page.getByRole("navigation", { name: "Pipeline views" }), `${workspace} shows the pipeline tabs`).toBeVisible();
        }
        // Let polls land, so the audit sees the page as people do.
        await page.waitForTimeout(800);
        // ...and any closing menu finish fading (a transition or a keyframe
        // animation; endless ones like a status pulse never end): mid-fade
        // text is not a contrast bug.
        await page
          .waitForFunction(() => document.getAnimations().every((animation) => animation.playState !== "running" || animation.effect?.getComputedTiming().iterations === Infinity), null, { timeout: 2_000 })
          .catch(() => undefined);
        const audit = await page.evaluate(auditLayout, { phone });
        const found = [
          ...(audit.horizontalScroll ? [`page scrolls sideways (${audit.scrollWidth} > ${audit.clientWidth}): ${audit.overflowing.join("; ")}`] : []),
          ...audit.selectClipped.map((item: string) => `select text under its arrow: ${item}`),
          ...audit.hairlineScroll.map((item: string) => `stray scrollbar: ${item}`),
          ...audit.misaligned.map((item: string) => `misaligned row: ${item}`),
          ...(phone ? audit.smallTargets.map((item: string) => `small touch target: ${item}`) : []),
          ...audit.lowContrast.map((item: string) => `text below AA contrast: ${item}`),
        ];
        problems.push(...found.map((item) => `${workspace}: ${item}`));
      }
      expect(problems, problems.join("\n")).toEqual([]);
    } finally {
      await page.request.delete(`/api/v1/pipelines/${pipelineId}`);
      await context.close();
    }
  });
}

test("layout: a page that wraps the console cannot turn its text black", async ({ browser }) => {
  // An artifact viewer or embed may style body; the console's text must
  // keep its own colour on the dark theme instead of inheriting that.
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: "dark" });
  const page = await context.newPage();
  const login = await page.request.post("/api/v1/auth/login", { data: { password: "admin" } });
  expect(login.ok()).toBe(true);
  try {
    await page.goto("/?w=fleet");
    await page.addStyleTag({ content: "body { color: #111; background: #fff; font-family: serif; }" });
    await expect(page.locator("main h1, main h2").first()).toBeVisible();
    await page
      .waitForFunction(() => document.getAnimations().every((animation) => animation.playState !== "running" || animation.effect?.getComputedTiming().iterations === Infinity), null, { timeout: 2_000 })
      .catch(() => undefined);
    const audit = await page.evaluate(auditLayout, { phone: false });
    expect(audit.lowContrast, audit.lowContrast.join("\n")).toEqual([]);
  } finally {
    await context.close();
  }
});
