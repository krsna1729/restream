import { expect, test } from "@playwright/test";

import { openSeededDashboard } from "./fixtures";

test("seed: media route body is owned by the v2 renderer @desktop", async ({
  page,
}) => {
  await openSeededDashboard(page, "mixed-health", "/?mode=media", {
    expectOverviewReady: false,
  });

  const media = page.locator("#dashboard-v2-media-content");
  await expect(media).toHaveAttribute("data-media-route-body", "v2");
  await expect(media.getByLabel("Search media library")).toBeVisible();
  await expect(media.getByLabel("Upload media file")).toBeVisible();
  await expect(media.locator("#media-library-results-summary")).toContainText(
    "media file",
  );
  await expect(
    page.locator("#media-mode-panel > #media-mode-content > *"),
  ).toHaveCount(0);
});
