import { defineConfig, devices } from '@playwright/test';

export default defineConfig({
    testDir: './test',
    testMatch: '**/*.spec.ts',
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
        {
            name: 'firefox-moq',
            grep: /program preview over MoQ/,
            use: {
                ...devices['Desktop Firefox'],
                launchOptions: {
                    firefoxUserPrefs: {
                        'media.autoplay.default': 0,
                        'media.autoplay.block-webaudio': false,
                    },
                },
            },
        },
    ],
});
