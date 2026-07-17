import { defineConfig, devices } from "@playwright/test";

const chromium = devices["Desktop Chrome"];

export default defineConfig({
  testDir: ".",
  testMatch: ["seed*.spec.ts", "visual-accessibility.spec.ts"],
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  workers: 1,
  reporter: "list",
  outputDir: "../../../.local/test-results/ui-redesign",
  snapshotPathTemplate: "{testDir}/snapshots/{projectName}/{arg}{ext}",
  timeout: 30_000,
  use: {
    baseURL: process.env.BASE_URL || "http://localhost:3030",
    colorScheme: "dark",
    locale: "en-US",
    screenshot: "only-on-failure",
    trace: "on-first-retry",
  },
  projects: [
    {
      name: "desktop-1440x900",
      use: { ...chromium, viewport: { width: 1440, height: 900 } },
    },
    {
      name: "tablet-1024x768",
      grepInvert: /@desktop/,
      use: { ...chromium, viewport: { width: 1024, height: 768 } },
    },
    {
      name: "mobile-390x844",
      grepInvert: /@desktop/,
      use: { ...chromium, viewport: { width: 390, height: 844 } },
    },
  ],
});
