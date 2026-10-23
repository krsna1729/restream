import { expect, test } from "@playwright/test";

// On-screen help against a running engine: the help drawer explains the
// page, its tour lights each step's element, and the onboarding tour moves
// across workspaces.

test("help: a page tour lights its elements, and onboarding walks the console", async ({ page }) => {
  const login = await page.request.post("/api/v1/auth/login", { data: { password: "admin" } });
  expect(login.ok()).toBe(true);

  await page.goto("/?w=fleet");
  await page.getByRole("button", { name: "Help for this page" }).click();
  const drawer = page.getByRole("dialog", { name: /plant healthy/ });
  await expect(drawer).toContainText("Delivering");
  await drawer.getByRole("button", { name: "Show me around this page" }).click();
  await expect(drawer).toHaveCount(0);

  const card = page.locator("[data-tour-overlay]").getByRole("dialog");
  await expect(card).toContainText("1 of");
  // The first step points at the page header: the spotlight is drawn round it.
  await expect(page.locator("[data-tour-overlay] .ring-2")).toBeVisible();
  await page.keyboard.press("ArrowRight");
  await expect(card).toContainText("2 of");
  await page.keyboard.press("Escape");
  await expect(card).toHaveCount(0);

  // `?` opens help anywhere; the full tour leaves Fleet for Channels.
  await page.keyboard.press("?");
  await page.getByRole("button", { name: "Take the full tour" }).click();
  await expect(card).toContainText("Welcome");
  for (let step = 0; step < 3; step += 1) await card.getByRole("button", { name: "Next" }).click();
  await expect(page).toHaveURL(/w=channels/);
  await expect(card).toContainText("Channels");
  await card.getByRole("button", { name: "End the tour" }).click();

  // Learn: a guide's step opens its page, and How it works explains a block.
  await page.goto("/?w=guides");
  await page.getByRole("button", { name: "Set up" }).click();
  await expect(page.getByRole("heading", { name: "Add a team member" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Go live on every platform" })).toHaveCount(0);
  await page.goto("/?w=architecture");
  await page.getByRole("button", { name: /Egress fabric/ }).click();
  await expect(page.locator('section[aria-labelledby="arch-detail"]')).toContainText("shard threads");
});
