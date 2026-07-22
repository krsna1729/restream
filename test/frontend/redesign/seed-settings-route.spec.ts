import { expect, test } from "@playwright/test";

import { openSeededDashboard } from "./fixtures";

test("seed: settings route body is owned by the v2 renderer @desktop", async ({
  page,
}) => {
  await openSeededDashboard(page, "mixed-health", "/?mode=settings", {
    expectOverviewReady: false,
  });

  const settings = page.locator("#dashboard-v2-settings-content");
  await expect(settings).toHaveAttribute("data-settings-route-body", "v2");
  await expect(
    settings.locator(
      '#dashboard-password-section[data-settings-v2-disclosure="dashboard-password-section"]',
    ),
  ).toBeVisible();
  await expect(
    settings.locator(
      '[data-settings-v2-disclosure-body="dashboard-password-section"]',
    ),
  ).toBeAttached();
  await expect(page.locator("#settings-mode-content")).toHaveCount(0);
});
