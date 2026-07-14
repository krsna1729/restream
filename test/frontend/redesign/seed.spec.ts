import { expect, test } from "@playwright/test";

import { openSeededDashboard } from "./fixtures";

test("seed: empty Overview is deterministic and canonical", async ({ page }) => {
  await openSeededDashboard(page, "empty");

  const overview = page.locator("#overview-mode-content");
  await expect(page).toHaveURL(/\?mode=overview$/);
  await expect(
    overview.getByRole("cell", { name: "No pipelines configured." }),
  ).toBeVisible();
  await expect(
    overview.getByRole("button", { name: "Add Pipeline", exact: true }),
  ).toBeVisible();
});

test("seed: mixed-health Overview exposes upstream and output state", async ({
  page,
}) => {
  await openSeededDashboard(page, "mixed-health");

  const overview = page.locator("#overview-mode-content");
  await expect(
    overview.getByRole("button", { name: "Healthy Program", exact: true }),
  ).toBeVisible();
  await expect(
    overview.getByRole("button", {
      name: "Retrying Destination",
      exact: true,
    }),
  ).toBeVisible();
  await expect(page.locator("#workspace-mode-summary")).toContainText(
    "1 retrying",
  );
});
