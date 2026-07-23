import { test, expect, type Page, type Locator } from '@playwright/test';
import fs from 'fs/promises';
import path from 'path';

interface DashboardPipelineTarget {
    id: string;
    name: string;
    stream_key: string;
    role: string;
}

interface PendingRequest {
    url: string;
    startTsMs: number;
}

interface CompletedRequest {
    url: string;
    durationMs: number;
    status: 'finished' | 'failed';
    errorText?: string;
}

const TEST_BASE_URL = process.env.BASE_URL || 'http://localhost:3030';
const TEST_PASSWORD = process.env.RESTREAM_UI_PASSWORD || 'admin';
const ARTIFACT_DIR =
    process.env.MSR_DASHBOARD_ARTIFACT_DIR || path.resolve('.local/artifacts/msr-dashboard');
const SUMMARY_JSON =
    process.env.MSR_DASHBOARD_SUMMARY_JSON || path.join(ARTIFACT_DIR, 'playwright-summary.json');
const RUNTIME_SECS =
    Math.max(60, Number.parseInt(process.env.MSR_DASHBOARD_RUNTIME_SECS || '1800', 10) || 1800);
const MAX_CHURN_OUTPUTS = Math.max(
    1,
    Number.parseInt(process.env.MSR_DASHBOARD_CHURN_OUTPUTS_PER_PIPELINE || '3', 10) || 3,
);
const DIAGNOSTICS_EVERY_CYCLES = Math.max(
    1,
    Number.parseInt(process.env.MSR_DASHBOARD_DIAGNOSTICS_EVERY_CYCLES || '3', 10) || 3,
);
const OUTPUT_RTMP_PORT =
    Number.parseInt(process.env.MSR_DASHBOARD_OUTPUT_RTMP_PORT || '1935', 10) || 1935;
const PIPELINES: DashboardPipelineTarget[] = JSON.parse(
    process.env.MSR_DASHBOARD_PIPELINES_JSON || '[]',
);

async function login(page: Page): Promise<void> {
    await page.goto('/login');
    await page.fill('#password-input', TEST_PASSWORD);
    await page.click('#login-btn');
    await page.waitForURL('**/');
}

async function selectPipelineInV2Selector(
    root: Locator,
    pipelineId: string,
    pipelineName: string,
): Promise<void> {
    await expect(root).toBeVisible();
    const compactSelector = root.getByLabel('Select pipeline');
    const pipelineButton = root.getByRole('button', { name: new RegExp(pipelineName) });
    await expect
        .poll(async () => (await compactSelector.count()) + (await pipelineButton.count()))
        .toBeGreaterThan(0);
    if ((await compactSelector.count()) > 0) {
        await expect(compactSelector).toBeVisible();
        await compactSelector.selectOption(pipelineId);
        return;
    }
    await pipelineButton.click();
}

async function selectPipeline(page: Page, target: DashboardPipelineTarget): Promise<void> {
    await page.goto('/?mode=pipeline');
    await selectPipelineInV2Selector(
        page.locator('#dashboard-v2-pipeline-selector-root'),
        target.id,
        target.name,
    );
    await expect(page.locator('#dashboard-v2-pipeline-output-overview-root')).toBeVisible({
        timeout: 30000,
    });
}

function outputOverview(page: Page): Locator {
    return page.locator('#dashboard-v2-pipeline-output-overview-root');
}

function outputCard(page: Page, outputName: string): Locator {
    return outputOverview(page).locator('article', { hasText: outputName }).first();
}

async function createChurnOutput(
    page: Page,
    target: DashboardPipelineTarget,
    outputName: string,
): Promise<Locator> {
    await selectPipeline(page, target);
    await outputOverview(page)
        .getByRole('button', { name: `Add output for ${target.name}` })
        .click();
    const modal = page.locator('#edit-out-modal');
    await expect(modal).toBeVisible();
    await modal.locator('#out-name-input').fill(outputName);
    await modal.locator('#out-protocol-input').selectOption('rtmp');
    await modal
        .locator('#out-rtmp-key-input')
        .fill(`rtmp://127.0.0.1:${OUTPUT_RTMP_PORT}/live/${outputName}`);
    await modal.locator('#out-submit-btn').click();
    await expect(modal).not.toBeVisible({ timeout: 30000 });
    const card = outputCard(page, outputName);
    await expect(card).toBeVisible({ timeout: 30000 });
    return card;
}

async function toggleOutput(card: Locator, toState: 'start' | 'stop'): Promise<void> {
    const toggle = card.getByRole('button', {
        name: toState === 'start' ? /^Start / : /^Stop /,
    });
    await expect(toggle).toBeVisible();
    await toggle.click();
    const nextState = card.getByRole('button', {
        name: toState === 'start' ? /^(Stop|Starting) / : /^(Start|Stopping) /,
    });
    await expect(nextState).toBeVisible({ timeout: 30000 });
}

async function deleteOutput(page: Page, card: Locator): Promise<void> {
    await card.getByRole('button', { name: /^More output actions for / }).click();
    await card.getByRole('menuitem', { name: /^Delete / }).click();
    const confirm = page.locator('#app-confirm-dialog button[value="confirm"]');
    await expect(confirm).toBeVisible({ timeout: 10000 });
    await confirm.click();
    await expect(card).toHaveCount(0, { timeout: 30000 });
}

async function runDiagnostics(page: Page, target: DashboardPipelineTarget): Promise<void> {
    await selectPipeline(page, target);
    await page.locator('#pipeline-workspace-tab-inspect').click();
    const select = page.locator('#inspect-pipeline-select');
    await expect(select).toBeVisible({ timeout: 30000 });
    await select.selectOption({ value: target.id });
    const openButton = page
        .locator('#dashboard-v2-pipeline-inspect-root')
        .getByRole('button', { name: 'Run diagnostics for inspected pipeline' });
    await expect(openButton).toBeEnabled({ timeout: 30000 });
    const responsePromise = page.waitForResponse(
        (response) =>
            response
                .url()
                .includes(
                    `/api/v1/pipelines/${encodeURIComponent(target.id)}/diagnostics/run`,
                ) && response.status() === 200,
        { timeout: 120000 },
    );
    await openButton.click();
    await responsePromise;
    const modal = page.locator('#diagnostics-modal');
    await expect(modal).toBeVisible({ timeout: 30000 });
    await expect(modal.locator('text=System Resources')).toBeVisible({
        timeout: 120000,
    });
    await expect(modal.locator('#diagnostics-total-time')).not.toHaveText('', {
        timeout: 30000,
    });
    await modal.locator('button', { hasText: 'Close' }).first().click();
    await expect(modal).not.toBeVisible({ timeout: 30000 });
}

test.describe.serial('MSR dashboard overnight soak', () => {
    test('keeps the dashboard hot while churning outputs and diagnostics', async ({
        page,
        context,
    }) => {
        test.setTimeout((RUNTIME_SECS + 600) * 1000);
        if (PIPELINES.length < 3) {
            throw new Error(`expected at least 3 dashboard pipelines, got ${PIPELINES.length}`);
        }

        await fs.mkdir(ARTIFACT_DIR, { recursive: true });
        const cdp = await context.newCDPSession(page);
        await cdp.send('Network.enable');

        const pending = new Map<string, PendingRequest>();
        const diagnosticsRequests: CompletedRequest[] = [];
        const failedRequests: CompletedRequest[] = [];

        cdp.on('Network.requestWillBeSent', (event) => {
            const url = String(event.request?.url || '');
            if (!url.includes('/api/v1/')) {
                return;
            }
            pending.set(event.requestId, {
                url,
                startTsMs: event.timestamp * 1000,
            });
        });
        cdp.on('Network.loadingFinished', (event) => {
            const request = pending.get(event.requestId);
            if (!request) {
                return;
            }
            pending.delete(event.requestId);
            if (request.url.includes('/diagnostics/run')) {
                diagnosticsRequests.push({
                    url: request.url,
                    durationMs: Math.max(0, event.timestamp * 1000 - request.startTsMs),
                    status: 'finished',
                });
            }
        });
        cdp.on('Network.loadingFailed', (event) => {
            const request = pending.get(event.requestId);
            if (!request) {
                return;
            }
            pending.delete(event.requestId);
            failedRequests.push({
                url: request.url,
                durationMs: Math.max(0, event.timestamp * 1000 - request.startTsMs),
                status: 'failed',
                errorText: event.errorText,
            });
        });

        await login(page);
        await page.goto('/?mode=pipeline');
        await expect(page.locator('#dashboard-v2-pipeline-selector-root')).toBeVisible({
            timeout: 30000,
        });

        const managedOutputs = new Map<string, string[]>();
        const diagnosticsByPipeline = new Map<string, number>();
        const cycleLatenciesMs: number[] = [];
        const startedAt = new Date().toISOString();
        const deadline = Date.now() + RUNTIME_SECS * 1000;
        let cycle = 0;

        while (Date.now() < deadline) {
            cycle += 1;
            for (const [index, target] of PIPELINES.entries()) {
                const cycleStart = Date.now();
                const managed = managedOutputs.get(target.id) || [];
                const outputName = `pw-soak-${cycle}-${index}-${Date.now()}`;
                const card = await createChurnOutput(page, target, outputName);
                await toggleOutput(card, 'start');
                managed.push(outputName);
                managedOutputs.set(target.id, managed);

                if (managed.length > MAX_CHURN_OUTPUTS) {
                    const oldest = managed.shift();
                    if (oldest) {
                        await selectPipeline(page, target);
                        const oldCard = outputCard(page, oldest);
                        await expect(oldCard).toBeVisible({ timeout: 30000 });
                        await toggleOutput(oldCard, 'stop');
                        await deleteOutput(page, oldCard);
                    }
                }

                if (cycle % DIAGNOSTICS_EVERY_CYCLES === 0) {
                    await runDiagnostics(page, target);
                    diagnosticsByPipeline.set(
                        target.name,
                        (diagnosticsByPipeline.get(target.name) || 0) + 1,
                    );
                } else {
                    await page.locator('#pipeline-workspace-tab-monitor').click();
                    const monitorSelect = page.locator('#control-room-pipeline-select');
                    if (await monitorSelect.isVisible().catch(() => false)) {
                        await monitorSelect.selectOption({ value: target.id });
                    }
                    await page.locator('#pipeline-workspace-tab-operate').click();
                }

                cycleLatenciesMs.push(Date.now() - cycleStart);
            }
        }

        const endedAt = new Date().toISOString();
        const diagnosticsDurationsMs = diagnosticsRequests.map((item) =>
            Math.round(item.durationMs),
        );
        const summary = {
            startedAt,
            endedAt,
            baseUrl: TEST_BASE_URL,
            runtimeSecs: RUNTIME_SECS,
            cyclesCompleted: cycle,
            pipelines: PIPELINES,
            managedOutputs: Object.fromEntries(managedOutputs.entries()),
            diagnosticsByPipeline: Object.fromEntries(diagnosticsByPipeline.entries()),
            diagnosticsDurationsMs,
            cycleLatenciesMs,
            failedRequests,
        };

        await fs.writeFile(SUMMARY_JSON, `${JSON.stringify(summary, null, 2)}\n`, 'utf8');

        expect(failedRequests, 'CDP observed failed API requests').toEqual([]);
        expect(
            diagnosticsDurationsMs.length,
            'expected at least one diagnostics run',
        ).toBeGreaterThan(0);
    });
});
