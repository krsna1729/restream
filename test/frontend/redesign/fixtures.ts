import { expect, type Page, type Route } from "@playwright/test";

import {
  operatorStates,
  type OperatorStateName,
} from "./fixtures/operator-states";

async function fulfillJson(route: Route, body: unknown): Promise<void> {
  await route.fulfill({
    status: 200,
    contentType: "application/json",
    body: JSON.stringify(body),
  });
}

async function login(page: Page): Promise<void> {
  await page.goto("/login");
  await page.locator("#password-input").fill("admin");
  await page.locator("#login-btn").click();
  await page.waitForURL(/\/$/);
}

export async function openSeededDashboard(
  page: Page,
  stateName: OperatorStateName,
  href = "/?mode=overview",
): Promise<void> {
  const fixture = operatorStates[stateName];
  await login(page);

  await page.addInitScript(() => {
    Object.defineProperty(window, "EventSource", {
      configurable: true,
      value: undefined,
    });
  });

  await page.route("**/api/v1/**", async (route) => {
    const url = new URL(route.request().url());
    switch (url.pathname) {
      case "/api/v1/logs/stream":
        await route.fulfill({
          status: 200,
          contentType: "text/event-stream",
          body: "retry: 60000\n\n",
        });
        return;
      case "/api/v1/settings":
        await fulfillJson(route, fixture.settings);
        return;
      case "/api/v1/dashboard/runtime":
        await fulfillJson(route, fixture.runtime);
        return;
      case "/api/v1/audio-caps":
        await fulfillJson(route, { caps: {}, platformLabels: {} });
        return;
      case "/api/v1/logs":
        await fulfillJson(route, { logs: fixture.logs });
        return;
      case "/api/v1/stream-keys":
        await fulfillJson(route, []);
        return;
      default:
        throw new Error(`Unmodeled redesign seed request: ${url.pathname}`);
    }
  });

  await page.goto(href);
  await expect(page.locator("#overview-mode-content h1")).toHaveText(
    "Operator Overview",
  );
}
