// Browser interop specs for Restream's MoQ origin and a moq-relay, driven by
// `cargo xtask harness moq-dev-interop -- --browsers`, which starts the
// endpoints and passes them in MOQ_BROWSER_* variables. Kept out of the
// frontend suite (its own package, `*.moq.ts` names) because nothing here
// works without those live endpoints.
import { defineConfig, devices } from '@playwright/test';

const pagePort = Number(process.env.MOQ_BROWSER_PAGE_PORT ?? '5180');
const resultsDir = '../../../.local/test-results/moq-browser';

export default defineConfig({
    testDir: '.',
    testMatch: '*.moq.ts',
    fullyParallel: false,
    forbidOnly: !!process.env.CI,
    retries: 0,
    workers: 1,
    reporter: [['list'], ['json', { outputFile: process.env.MOQ_BROWSER_REPORT ?? `${resultsDir}/report.json` }]],
    outputDir: process.env.MOQ_BROWSER_OUTPUT_DIR ?? resultsDir,
    timeout: 60_000,
    use: {
        baseURL: `http://localhost:${pagePort}`,
        trace: 'retain-on-failure',
        screenshot: 'only-on-failure',
    },
    webServer: {
        command: `npx vite --config vite.config.ts --port ${pagePort} --strictPort`,
        url: `http://localhost:${pagePort}/index.html`,
        reuseExistingServer: false,
        timeout: 60_000,
    },
    projects: [
        {
            name: 'chromium',
            use: {
                ...devices['Desktop Chrome'],
                // Full Chromium (new headless) rather than the headless shell:
                // the shell lacks the media stack WebCodecs decoding needs.
                channel: 'chromium',
                launchOptions: { args: ['--autoplay-policy=no-user-gesture-required'] },
            },
        },
        {
            name: 'firefox',
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
