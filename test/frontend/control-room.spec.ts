import { expect, test } from "@playwright/test";

// The control room against a running engine: Fleet follows the runtime state
// stream, so a change made through the API shows without a reload.

test("fleet ranks a stalled pipeline and clears it when the output stops", async ({
  page,
}) => {
  const login = await page.request.post("/api/v1/auth/login", {
    data: { password: "admin" },
  });
  expect(login.ok()).toBe(true);

  const name = `cr-fleet-${Date.now()}`;
  const created = await page.request.post("/api/v1/pipelines", {
    data: { name, streamKey: `crfleet_${Date.now()}` },
  });
  expect(created.ok()).toBe(true);
  const pipelineId: string = (await created.json()).pipeline.id;

  try {
    const output = await page.request.post(
      `/api/v1/pipelines/${pipelineId}/outputs`,
      {
        data: {
          name: "cr-out",
          url: "rtmp://127.0.0.1:9/live/cr",
          config: { video: { mode: "source" }, audio: { mode: "all" } },
        },
      },
    );
    expect(output.ok()).toBe(true);
    const outputId: string = (await output.json()).output.id;
    const start = await page.request.post(
      `/api/v1/pipelines/${pipelineId}/outputs/${outputId}/start`,
    );
    expect(start.ok()).toBe(true);

    // An unknown workspace opens Fleet.
    await page.goto("/control-room.html?w=unknown");
    await expect(
      page.getByRole("navigation", { name: "Workspaces" }).getByRole("link", {
        name: "Fleet",
      }),
    ).toHaveAttribute("aria-current", "page");

    // No publisher: one upstream cause for the pipeline's wanted output.
    const attention = page.getByRole("region", { name: /Needs attention/ });
    const item = attention.getByRole("listitem").filter({ hasText: name });
    await expect(item).toContainText("ingest: no publisher connected");
    await expect(item).toContainText("1 output: cr-out");
    const row = page.getByRole("row").filter({ hasText: name });
    await expect(row).toContainText("no input");
    await expect(row).toContainText("0 / 1");

    const stop = await page.request.post(
      `/api/v1/pipelines/${pipelineId}/outputs/${outputId}/stop`,
    );
    expect(stop.ok()).toBe(true);
    // A stopped output is not wanted: the cause is gone, with no reload.
    await expect(item).toHaveCount(0);
    await expect(row).toContainText("0 / 0");
  } finally {
    await page.request.delete(`/api/v1/pipelines/${pipelineId}`);
  }
});
