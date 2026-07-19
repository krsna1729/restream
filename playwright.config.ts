import { defineConfig, devices } from '@playwright/test';

const testIgnore = [
    ...(process.env.PLAYWRIGHT_BROWSER_DOM_HARNESS
        ? []
        : [
              '**/frontend-browser-dom.spec.ts',
              '**/redesign/visual-accessibility.spec.ts',
          ]),
    ...(process.env.MSR_DASHBOARD_PLAYWRIGHT ? [] : ['**/msr-dashboard-soak.spec.ts']),
];

export default defineConfig({
    testDir: './test',
    testMatch: '**/*.spec.ts',
    testIgnore,
    fullyParallel: false,
    forbidOnly: !!process.env.CI,
    retries: process.env.CI ? 1 : 0,
    workers: 1,
    reporter: 'list',
    outputDir: './.local/test-results',
    timeout: 30000,
    use: {
        baseURL: process.env.BASE_URL || 'http://localhost:3030',
        trace: 'on-first-retry',
        screenshot: 'only-on-failure',
    },
    projects: [
        {
            name: 'chromium',
            use: { ...devices['Desktop Chrome'] },
        },
    ],
});
