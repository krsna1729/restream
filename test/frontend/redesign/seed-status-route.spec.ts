import { expect, test } from "@playwright/test";

import { openSeededDashboard } from "./fixtures";

test("seed: status route body is owned by the v2 renderer @desktop", async ({
  page,
}) => {
  await openSeededDashboard(page, "mixed-health", "/?mode=status", {
    expectOverviewReady: false,
  });

  const status = page.locator("#dashboard-v2-status-content");
  await expect(status).toHaveAttribute("data-status-route-body", "v2");
  await expect(
    status.getByRole("button", { name: "Refresh status data" }),
  ).toBeVisible();
  await expect(status.locator("#status-route-summary")).toContainText(
    "Status loaded for seeded",
  );
  await expect(page.locator("#status-mode-content")).toHaveCount(0);
});
