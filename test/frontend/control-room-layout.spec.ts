import { expect, test } from "@playwright/test";

import { auditLayout } from "../../scripts/dev/frontend/layout-audit.mjs";

// Layout regressions on every workspace, at a desktop and a phone width:
// no sideways page scroll, no stray scrollbars, no select text under its
// arrow, controls on one row at one height and centre, touch-sized targets
// on the phone, and the pipeline tabs on every pipeline view (Logs too).
// The same audit drives the screenshot matrix (scripts/dev/frontend/shots.mjs).

const WORKSPACES = [
  "fleet",
  "channels",
  "pipelines",
  "operate",
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

for (const width of [1440, 375]) {
  test(`layout: every workspace at ${width} px`, async ({ browser }) => {
    test.setTimeout(180_000);
    const phone = width < 768;
    const context = await browser.newContext({ viewport: { width, height: 900 }, hasTouch: phone, isMobile: phone });
    const page = await context.newPage();
    const login = await page.request.post("/api/v1/auth/login", { data: { password: "admin" } });
    expect(login.ok()).toBe(true);
    const stamp = Date.now();
    const created = await page.request.post("/api/v1/pipelines", { data: { name: `cr-layout-${width}-${stamp}`, streamKey: `crlayout${width}_${stamp}` } });
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
        const audit = await page.evaluate(auditLayout, { phone });
        const found = [
          ...(audit.horizontalScroll ? [`page scrolls sideways (${audit.scrollWidth} > ${audit.clientWidth}): ${audit.overflowing.join("; ")}`] : []),
          ...audit.selectClipped.map((item: string) => `select text under its arrow: ${item}`),
          ...audit.hairlineScroll.map((item: string) => `stray scrollbar: ${item}`),
          ...audit.misaligned.map((item: string) => `misaligned row: ${item}`),
          ...(phone ? audit.smallTargets.map((item: string) => `small touch target: ${item}`) : []),
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
