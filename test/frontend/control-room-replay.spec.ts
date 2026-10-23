import { expect, test } from "@playwright/test";

// Stability Lab's replay over a span the operator chooses: the span rides in
// the URL, the presets and zoom buttons change it, and a following span
// ends at "now".

test("replay: choose, widen and narrow the span; the URL keeps it", async ({ page }) => {
  const login = await page.request.post("/api/v1/auth/login", { data: { password: "admin" } });
  expect(login.ok()).toBe(true);
  const name = `cr-replay-${Date.now()}`;
  const created = await page.request.post("/api/v1/pipelines", { data: { name, streamKey: `crreplay_${Date.now()}` } });
  expect(created.ok()).toBe(true);
  const pipelineId: string = (await created.json()).pipeline.id;
  try {
    // The recorder samples every pipeline every 2 s; wait for its first.
    await expect
      .poll(async () => (await page.request.get(`/api/v1/pipelines/${pipelineId}/flight-recorder`)).status(), { timeout: 15_000 })
      .toBe(200);
    // A bounded window from the URL (as an incident's Replay opens it).
    const now = Date.now();
    await page.goto(`/?w=stability&p=${pipelineId}&from=${now - 6 * 60_000}&to=${now - 60_000}`);
    const history = page.locator('section[aria-labelledby="replay-window"]');
    await expect(history).toContainText("5 min");
    await expect(page.getByRole("button", { name: /^(Play|Replay)$/ })).toBeVisible();

    await history.getByRole("button", { name: "Last 10 min" }).click();
    await expect(history).toContainText("→ now · 10 min");
    await expect(page).not.toHaveURL(/[?&]to=/);
    await history.getByRole("button", { name: "Show a longer span" }).click();
    await expect(history).toContainText("→ now · 20 min");
    await history.getByRole("button", { name: "Show a shorter span" }).click();
    await history.getByRole("button", { name: "Show a shorter span" }).click();
    await expect(history).toContainText("→ now · 5 min");

    // The span survives a reload: it is the URL's.
    await page.reload();
    await expect(page.locator('section[aria-labelledby="replay-window"]')).toContainText("→ now · 5 min");
  } finally {
    await page.request.delete(`/api/v1/pipelines/${pipelineId}`);
  }
});
