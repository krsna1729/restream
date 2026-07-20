import { test, expect, type Locator, type Page, request } from '@playwright/test';
import { spawn, type ChildProcess } from 'child_process';
import path from 'path';
import { fileURLToPath } from 'url';

const TEST_BASE_URL = process.env.BASE_URL || 'http://localhost:3030';
const TEST_DIR = path.dirname(fileURLToPath(import.meta.url));

async function login(page: Page): Promise<void> {
    await page.goto('/login');
    await page.fill('#password-input', 'admin');
    await page.click('#login-btn');
    await page.waitForURL('**/');
}

// v2 is the default dashboard UI; these specs exercise the legacy player DOM
// (#pipelines, #pipe-info-col, #video-player, etc.) specifically, so pin the
// UI version explicitly rather than relying on whatever the default happens
// to be. Only "v1"/"v2" are recognized values that get written to the
// persisted localStorage preference (see resolveDashboardUiVersion in
// dashboard-v2-loader.ts); any other value only takes effect for the current
// navigation and does NOT persist, so a later bare page.goto('/') elsewhere
// in the same test would silently fall back to the v2 default again.
async function loginToLegacyDashboard(page: Page): Promise<void> {
    await login(page);
    await page.goto('/?ui=v1');
}

async function openPipelineWorkspace(page: Page): Promise<void> {
    const tab = page.locator('#workspace-tab-pipeline');
    await expect(tab).toBeVisible();
    await tab.click();
}

async function selectPipelineForWorkspace(
    page: Page,
    pipelineName: string,
): Promise<void> {
    await openPipelineWorkspace(page);
    const pipeline = page.locator('#pipelines li', { hasText: pipelineName });
    await expect(pipeline).toBeVisible({ timeout: 10000 });
    await pipeline.click();
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

async function waitFor<T>(
    fn: () => Promise<T>,
    predicate: (value: T) => boolean,
    attempts = 40,
    delayMs = 1000,
): Promise<T> {
    let lastValue: T | undefined;
    for (let attempt = 0; attempt < attempts; attempt++) {
        lastValue = await fn();
        if (predicate(lastValue)) {
            return lastValue;
        }
        if (attempt + 1 < attempts) {
            await new Promise((resolve) => setTimeout(resolve, delayMs));
        }
    }
    if (lastValue !== undefined) {
        return lastValue;
    }
    throw new Error('waitFor called without any attempts');
}

async function expectPreviewPlayback(video: Locator): Promise<void> {
    await expect.poll(
        async () => video.evaluate((element) => {
            const videoElement = element as HTMLVideoElement;
            return {
                advanced: videoElement.currentTime > 0,
                paused: videoElement.paused,
                playable: videoElement.readyState >= 2,
            };
        }),
        {
            message: 'expected HLS preview media to advance beyond metadata loading',
            timeout: 30000,
        },
    ).toEqual({
        advanced: true,
        paused: false,
        playable: true,
    });
}

// Shared by the legacy (#video-player) and v2
// (#dashboard-v2-pipeline-input-status-root) alternate-audio tests: both
// containers mount the same renderInputPreview() component from
// web/ts/features/input-preview.ts, so the audio-track picker markup and
// behavior are identical — only the surrounding container differs.
async function verifyAlternateAudioTrackSwitch(
    page: Page,
    previewContainer: Locator,
    pipelineId: string,
): Promise<void> {
    const requestedAudio15Urls = new Set<string>();
    const responseListener = (response: { url(): string; status(): number }) => {
        const url = response.url();
        if (response.status() === 200 && url.includes(`/hls/${pipelineId}/audio/15/`)) {
            requestedAudio15Urls.add(url);
        }
    };
    page.on('response', responseListener);

    const video = previewContainer.locator('video[data-role="input-preview-video"]');
    await expect(video).toBeAttached();

    const playBtn = previewContainer.getByRole('button', { name: /Play (input )?preview/ });
    await expect(playBtn).toBeVisible();
    await playBtn.click();

    const usesHlsJs = await page.evaluate(() => !!window.Hls);
    if (usesHlsJs) {
        await expect(video).toHaveAttribute('src', /blob:/, { timeout: 20000 });
    } else {
        await expect(video).toHaveAttribute('src', new RegExp(`/hls/${pipelineId}/master.m3u8`), { timeout: 20000 });
    }

    const audioPickerButton = previewContainer.locator('button[aria-haspopup="listbox"]');
    await expect(audioPickerButton).toBeVisible({ timeout: 20000 });

    await expect.poll(
        () => video.evaluate((element) => {
            const videoEl = element as HTMLVideoElement;
            return videoEl.readyState >= 2 && videoEl.currentTime > 0 && !videoEl.error;
        }),
        { timeout: 20000 },
    ).toBe(true);

    const playbackBeforeSwitch = await video.evaluate((element) => {
        const videoEl = element as HTMLVideoElement;
        return {
            currentTime: videoEl.currentTime,
            readyState: videoEl.readyState,
            paused: videoEl.paused,
            errorCode: videoEl.error?.code ?? null,
        };
    });
    expect(playbackBeforeSwitch.readyState).toBeGreaterThanOrEqual(2);
    expect(playbackBeforeSwitch.currentTime).toBeGreaterThan(0);
    expect(playbackBeforeSwitch.paused).toBe(false);
    expect(playbackBeforeSwitch.errorCode).toBeNull();

    const beforeLabel = await audioPickerButton.textContent();

    await audioPickerButton.click();
    const options = page.locator('[role="option"]');
    await expect(options).toHaveCount(16, { timeout: 20000 });
    await options.last().click();

    if (beforeLabel) {
        await expect(audioPickerButton).not.toHaveText(beforeLabel, { timeout: 10000 });
    }

    await audioPickerButton.click();
    await expect(options.last()).toHaveAttribute('aria-selected', 'true');

    await expect.poll(
        () => video.evaluate((element) => {
            const videoEl = element as HTMLVideoElement;
            return Boolean(videoEl.currentTime > 0 && !videoEl.paused && !videoEl.error);
        }),
        { timeout: 20000 },
    ).toBe(true);

    await expect.poll(
        () => requestedAudio15Urls.size,
        { timeout: 20000 },
    ).toBeGreaterThan(0);

    const playbackAfterSwitch = await video.evaluate((element) => {
        const videoEl = element as HTMLVideoElement;
        return {
            currentTime: videoEl.currentTime,
            readyState: videoEl.readyState,
            paused: videoEl.paused,
            errorCode: videoEl.error?.code ?? null,
        };
    });
    expect(playbackAfterSwitch.readyState).toBeGreaterThanOrEqual(2);
    expect(playbackAfterSwitch.currentTime).toBeGreaterThan(playbackBeforeSwitch.currentTime);
    expect(playbackAfterSwitch.paused).toBe(false);
    expect(playbackAfterSwitch.errorCode).toBeNull();

    page.off('response', responseListener);
}

test.describe('HLS Player — pure helpers', () => {
    test.beforeEach(async ({ page }) => {
        await login(page);
    });

    test('formatPreviewSampleRate handles various inputs', async ({ page }) => {
        const result = await page.evaluate(() => {
            const fn = (rate: number | null | undefined): string | null => {
                if (!Number.isFinite(rate) || !rate) return null;
                const khz = rate / 1000;
                return `${Number.isInteger(khz) ? khz.toFixed(0) : khz.toFixed(1)} kHz`;
            };
            return {
                null: fn(null),
                undefined: fn(undefined),
                zero: fn(0),
                negative: fn(-1000),
                intKhz: fn(48000),
                floatKhz: fn(44100),
                highRate: fn(96000),
            };
        });
        expect(result.null).toBeNull();
        expect(result.undefined).toBeNull();
        expect(result.zero).toBeNull();
        expect(result.negative).toBe('-1 kHz');
        expect(result.intKhz).toBe('48 kHz');
        expect(result.floatKhz).toBe('44.1 kHz');
        expect(result.highRate).toBe('96 kHz');
    });

    test('getFriendlyAudioTrackName filters generic names', async ({ page }) => {
        const result = await page.evaluate(() => {
            const fn = (name: string | null | undefined): string | null => {
                const trimmedName = (name || '').trim();
                if (!trimmedName || /^audio\d+$/i.test(trimmedName)) return null;
                return trimmedName;
            };
            return {
                null: fn(null),
                undefined: fn(undefined),
                empty: fn(''),
                generic: fn('audio1'),
                genericUpper: fn('AUDIO2'),
                friendly: fn('English'),
                friendlyWithSpaces: fn('  Commentary  '),
                numeric: fn('audio123'),
            };
        });
        expect(result.null).toBeNull();
        expect(result.undefined).toBeNull();
        expect(result.empty).toBeNull();
        expect(result.generic).toBeNull();
        expect(result.genericUpper).toBeNull();
        expect(result.numeric).toBeNull();
        expect(result.friendly).toBe('English');
        expect(result.friendlyWithSpaces).toBe('Commentary');
    });

    test('getPreviewAudioMetadata matches track by index and position', async ({ page }) => {
        const result = await page.evaluate(async () => {
            const tracks = [
                { index: 0, codec: 'aac', channels: 2, sample_rate: 48000 },
                { index: 2, codec: 'opus', channels: 1, sample_rate: 48000 },
            ];
            const { getPreviewAudioMetadata } = await import('/js/features/input-preview.js');
            const pipe = { input: { audioTracks: tracks } };
            return {
                matchByIndex: getPreviewAudioMetadata(pipe as never, 0)?.codec,
                matchByPosition: getPreviewAudioMetadata(pipe as never, 1)?.codec,
                noMatch: getPreviewAudioMetadata(pipe as never, 99),
            };
        });
        expect(result.matchByIndex).toBe('aac');
        expect(result.matchByPosition).toBe('opus');
        expect(result.noMatch).toBeNull();
    });

    test('getPreviewAudioMetadata preserves 16-track and sparse-track mappings', async ({ page }) => {
        const result = await page.evaluate(async () => {
            const { getPreviewAudioMetadata } = await import('/js/features/input-preview.js');
            const denseTracks = Array.from({ length: 16 }, (_, index) => ({
                index,
                codec: 'aac',
                channels: index % 2 === 0 ? 2 : 1,
                sample_rate: 48000,
                language: `lang${index}`,
            }));
            const sparseTracks = [0, 2, 5, 15].map((index) => ({
                index,
                codec: 'aac',
                channels: 2,
                sample_rate: 48000,
                language: `lang${index}`,
            }));
            return {
                denseLastIndex: getPreviewAudioMetadata({ input: { audioTracks: denseTracks } } as never, 15)?.index,
                denseLastLanguage: getPreviewAudioMetadata({ input: { audioTracks: denseTracks } } as never, 15)?.language,
                sparseExactIndex: getPreviewAudioMetadata({ input: { audioTracks: sparseTracks } } as never, 15)?.index,
                sparseFallbackIndex: getPreviewAudioMetadata({ input: { audioTracks: sparseTracks } } as never, 3)?.index,
                sparseFallbackLanguage: getPreviewAudioMetadata({ input: { audioTracks: sparseTracks } } as never, 3)?.language,
            };
        });

        expect(result.denseLastIndex).toBe(15);
        expect(result.denseLastLanguage).toBe('lang15');
        expect(result.sparseExactIndex).toBe(15);
        expect(result.sparseFallbackIndex).toBe(15);
        expect(result.sparseFallbackLanguage).toBe('lang15');
    });

    test('formatCodecName returns friendly names', async ({ page }) => {
        const result = await page.evaluate(() => {
            const fn = (codec: string | undefined | null): string | null => {
                if (!codec) return null;
                const c = String(codec).toLowerCase().replace(/[^a-z0-9]/g, '');
                if (c === 'h264' || c === 'avc' || c === 'avc1') return 'H.264';
                if (c === 'h265' || c === 'hevc' || c === 'hvc1') return 'H.265';
                if (c === 'aac') return 'AAC';
                if (c === 'mp3' || c === 'mp3float') return 'MP3';
                if (c === 'opus') return 'Opus';
                if (c === 'vp8') return 'VP8';
                if (c === 'vp9') return 'VP9';
                if (c === 'av1') return 'AV1';
                return codec;
            };
            return {
                h264: fn('h264'),
                avc: fn('AVC'),
                hevc: fn('HEVC'),
                aac: fn('AAC'),
                opus: fn('Opus'),
                unknown: fn('unknown-codec'),
                null: fn(null),
            };
        });
        expect(result.h264).toBe('H.264');
        expect(result.avc).toBe('H.264');
        expect(result.hevc).toBe('H.265');
        expect(result.aac).toBe('AAC');
        expect(result.opus).toBe('Opus');
        expect(result.unknown).toBe('unknown-codec');
        expect(result.null).toBeNull();
    });

    test('formatChannelCount returns correct labels', async ({ page }) => {
        const result = await page.evaluate(() => {
            const fn = (n: number): string => {
                if (n === 1) return 'Mono (1 ch)';
                if (n === 2) return 'Stereo (2 ch)';
                if (n === 6) return '5.1 (6 ch)';
                if (n === 8) return '7.1 (8 ch)';
                return `${n} ch`;
            };
            return {
                mono: fn(1),
                stereo: fn(2),
                surround: fn(6),
                atmos: fn(8),
                other: fn(3),
            };
        });
        expect(result.mono).toBe('Mono (1 ch)');
        expect(result.stereo).toBe('Stereo (2 ch)');
        expect(result.surround).toBe('5.1 (6 ch)');
        expect(result.atmos).toBe('7.1 (8 ch)');
        expect(result.other).toBe('3 ch');
    });

    test('buildInputPreviewUrl constructs correct HLS URL', async ({ page }) => {
        const result = await page.evaluate(async () => {
            const { buildInputPreviewUrl } = await import('/js/features/input-preview.js');
            return {
                simple: buildInputPreviewUrl('abc123'),
                specialChars: buildInputPreviewUrl('pipe/id+1'),
                unicode: buildInputPreviewUrl('pipeline-ñ'),
            };
        });
        expect(result.simple).toBe('/hls/abc123/master.m3u8');
        expect(result.specialChars).toBe('/hls/pipe%2Fid%2B1/master.m3u8');
        expect(result.unicode).toBe('/hls/pipeline-%C3%B1/master.m3u8');
    });
});
test.describe('HLS Player — DOM rendering', () => {
    test.beforeEach(async ({ page }) => {
        await login(page);
    });

    test('player container exists in DOM but is hidden until pipeline selected', async ({ page }) => {
        const playerElem = page.locator('#video-player');
        await expect(playerElem).toBeAttached();
        await expect(playerElem).toBeEmpty();
        const parentCol = page.locator('#pipe-info-col');
        await expect(parentCol).toHaveClass(/hidden/);
    });

    test('renderInputPreview creates video element and overlay', async ({ page }) => {
        const result = await page.evaluate(async () => {
            const container = document.getElementById('video-player');
            if (!container) return { error: 'no container' };

            const pipe = {
                id: 'test-pipe-1',
                name: 'Test Pipeline',
                key: 'test_key_abc123',
                inputSource: null,
                ingestUrls: { rtmp: null, srt: null },
                input: {
                    status: 'on',
                    time: null,
                    video: { codec: 'h264', width: 1920, height: 1080, fps: 30 },
                    audio: { codec: 'aac', channels: 2, sample_rate: 48000 },
                    audioTracks: [{ index: 0, codec: 'aac', channels: 2, sample_rate: 48000 }],
                    bytesReceived: 0,
                    bytesSent: 0,
                    readers: 0,
                    bitrateKbps: null,
                    publisher: null,
                    unexpectedReadersCount: 0,
                },
                outs: [],
                stats: {
                    inputBitrateKbps: null,
                    outputBitrateKbps: null,
                    readerCount: 0,
                    outputCount: 0,
                    readerMismatch: false,
                    unexpectedReadersCount: 0,
                },
                recording: { enabled: false, active: false },
            };

            const { renderInputPreview } = await import('/js/features/input-preview.js');
            renderInputPreview(container, pipe);

            const video = container.querySelector('video');
            const shell = container.firstElementChild;
            const buttons = container.querySelectorAll('button');
            const playBtn = Array.from(buttons).find(b => b.textContent?.trim() === 'Play preview') || null;

            return {
                shellExists: !!shell,
                videoExists: !!video,
                videoRole: video?.getAttribute('data-role'),
                videoMuted: video?.muted,
                videoPlaysInline: video?.playsInline,
                videoPreload: video?.getAttribute('preload'),
                videoPreviewSrc: video?.dataset.previewSrc,
                overlayExists: !!playBtn,
                playButtonText: playBtn?.textContent?.trim() || null,
                containerDataset: container.dataset.previewSrc,
            };
        });

        expect(result.error).toBeUndefined();
        expect(result.shellExists).toBe(true);
        expect(result.videoExists).toBe(true);
        expect(result.videoRole).toBe('input-preview-video');
        expect(result.videoMuted).toBe(true);
        expect(result.videoPlaysInline).toBe(true);
        expect(result.videoPreload).toBe('none');
        expect(result.videoPreviewSrc).toContain('/hls/test-pipe-1/master.m3u8');
        expect(result.overlayExists).toBe(true);
        expect(result.playButtonText).toBe('Play preview');
        expect(result.containerDataset).toContain('/hls/test-pipe-1/master.m3u8');
    });

    test('renderInputPreview shows message when pipeline has no key', async ({ page }) => {
        const result = await page.evaluate(async () => {
            const container = document.getElementById('video-player');
            if (!container) return { error: 'no container' };

            const pipe = {
                id: 'no-key-pipe',
                name: 'No Key',
                key: null,
                inputSource: null,
                ingestUrls: { rtmp: null, srt: null },
                input: {
                    status: 'on',
                    time: null,
                    video: null,
                    audio: null,
                    audioTracks: [],
                    bytesReceived: 0,
                    bytesSent: 0,
                    readers: 0,
                    bitrateKbps: null,
                    publisher: null,
                    unexpectedReadersCount: 0,
                },
                outs: [],
                stats: {
                    inputBitrateKbps: null,
                    outputBitrateKbps: null,
                    readerCount: 0,
                    outputCount: 0,
                    readerMismatch: false,
                    unexpectedReadersCount: 0,
                },
                recording: { enabled: false, active: false },
            };

            const { renderInputPreview } = await import('/js/features/input-preview.js');
            renderInputPreview(container, pipe);

            return {
                messageText: container.querySelector('p')?.textContent || null,
                hasVideo: !!container.querySelector('video'),
            };
        });

        expect(result.error).toBeUndefined();
        expect(result.messageText).toContain('stream key is not assigned');
        expect(result.hasVideo).toBe(false);
    });

    test('clearInputPreview removes video and cleans up', async ({ page }) => {
        const result = await page.evaluate(async () => {
            const container = document.getElementById('video-player');
            if (!container) return { error: 'no container' };

            const pipe = {
                id: 'clear-test',
                name: 'Clear Test',
                key: 'test_key',
                inputSource: null,
                ingestUrls: { rtmp: null, srt: null },
                input: {
                    status: 'on',
                    time: null,
                    video: null,
                    audio: null,
                    audioTracks: [],
                    bytesReceived: 0,
                    bytesSent: 0,
                    readers: 0,
                    bitrateKbps: null,
                    publisher: null,
                    unexpectedReadersCount: 0,
                },
                outs: [],
                stats: {
                    inputBitrateKbps: null,
                    outputBitrateKbps: null,
                    readerCount: 0,
                    outputCount: 0,
                    readerMismatch: false,
                    unexpectedReadersCount: 0,
                },
                recording: { enabled: false, active: false },
            };

            const { renderInputPreview, clearInputPreview } = await import('/js/features/input-preview.js');

            // Don't set previewSrc before — let renderInputPreview set it
            renderInputPreview(container, pipe);

            const videoBefore = container.querySelector('video');
            const hasVideoBefore = !!videoBefore;

            clearInputPreview(container);

            const videoAfter = container.querySelector('video');
            return {
                hasVideoBefore,
                hasVideoAfter: !!videoAfter,
                containerEmpty: container.children.length === 0,
                previewSrcCleared: !container.dataset.previewSrc,
            };
        });

        expect(result.error).toBeUndefined();
        expect(result.hasVideoBefore).toBe(true);
        expect(result.hasVideoAfter).toBe(false);
        expect(result.containerEmpty).toBe(true);
        expect(result.previewSrcCleared).toBe(true);
    });

    test('renderInputPreview is idempotent for same pipeline', async ({ page }) => {
        const result = await page.evaluate(async () => {
            const container = document.getElementById('video-player');
            if (!container) return { error: 'no container' };

            const pipe = {
                id: 'idempotent-test',
                name: 'Idempotent',
                key: 'test_key',
                inputSource: null,
                ingestUrls: { rtmp: null, srt: null },
                input: {
                    status: 'on',
                    time: null,
                    video: null,
                    audio: null,
                    audioTracks: [],
                    bytesReceived: 0,
                    bytesSent: 0,
                    readers: 0,
                    bitrateKbps: null,
                    publisher: null,
                    unexpectedReadersCount: 0,
                },
                outs: [],
                stats: {
                    inputBitrateKbps: null,
                    outputBitrateKbps: null,
                    readerCount: 0,
                    outputCount: 0,
                    readerMismatch: false,
                    unexpectedReadersCount: 0,
                },
                recording: { enabled: false, active: false },
            };

            const { renderInputPreview } = await import('/js/features/input-preview.js');

            renderInputPreview(container, pipe);
            const childrenAfterFirstCall = container.children.length;
            const previewSrcAfterFirstCall = container.dataset.previewSrc;

            renderInputPreview(container, pipe);
            const childrenAfterSecondCall = container.children.length;

            return {
                childrenAfterFirstCall,
                previewSrcAfterFirstCall,
                childrenAfterSecondCall,
                sameChildren: childrenAfterFirstCall === childrenAfterSecondCall,
            };
        });

        expect(result.error).toBeUndefined();
        expect(result.childrenAfterFirstCall).toBeGreaterThan(0);
        expect(result.childrenAfterSecondCall).toBeGreaterThan(0);
        expect(result.sameChildren).toBe(true);
    });

});

test.describe('HLS Player — integration', () => {
    test.beforeEach(async ({ page }) => {
        await login(page);
    });

    test('player page loads successfully after login', async ({ page }) => {
        await expect(page.locator('body')).toBeVisible();
    });

    test('dashboard has video-player container (hidden by default)', async ({ page }) => {
        const playerContainer = page.locator('#video-player');
        await expect(playerContainer).toBeAttached();
        await expect(playerContainer).toBeEmpty();
    });

    test('health endpoint is reachable', async ({ page }) => {
        const response = await page.request.get('/healthz');
        expect(response.ok()).toBe(true);
    });

    test('HLS playlist endpoint returns 404 for nonexistent pipeline', async ({ page }) => {
        const response = await page.request.get('/hls/nonexistent/index.m3u8');
        expect(response.status()).toBe(404);
    });

    test('HLS segment endpoint returns 404 for nonexistent pipeline', async ({ page }) => {
        const response = await page.request.get('/hls/nonexistent/seg1.m4s');
        expect(response.status()).toBe(404);
    });
});

test.describe.serial('HLS Player — live playback', () => {
    let livePipelineId: string;
    let ffmproc: ChildProcess | null = null;
    const INPUT_FILE = path.resolve(
        TEST_DIR,
        '..',
        'fixtures',
        'media-library',
        'colorbar-timer-2v16a.mp4',
    );

    let livePipelineName: string;

    test.beforeAll(async () => {
        const ctx = await request.newContext({ baseURL: TEST_BASE_URL });

        // login
        await ctx.post('/api/v1/auth/login', { data: { password: 'admin' } });

        // create pipeline
        livePipelineName = `PlaywrightHls_${Date.now()}`;
        const pipeKey = `pw_hls_${Date.now()}`;
        const createResp = await ctx.post('/api/v1/pipelines', {
            data: { name: livePipelineName, streamKey: pipeKey },
        });
        expect(createResp.ok()).toBe(true);
        const pipeJson = await createResp.json();
        livePipelineId = pipeJson.pipeline.id;
        expect(livePipelineId).toBeTruthy();

        // start ffmpeg publisher (RTMP)
        const target = `rtmp://127.0.0.1:1935/live/${pipeKey}`;
        ffmproc = spawn('ffmpeg', [
            '-nostdin', '-re', '-stream_loop', '-1',
            '-i', INPUT_FILE,
            '-map', '0:v:0', '-map', '0:a:0',
            '-c', 'copy', '-f', 'flv', target,
        ], { stdio: ['ignore', 'pipe', 'pipe'] });
        ffmproc.on('error', (err) => {
            console.error('ffmpeg spawn error:', err.message);
        });

        // wait for pipeline input to go "on"
        for (let i = 0; i < 30; i++) {
            const healthResp = await ctx.get('/api/v1/engine/health');
            if (!healthResp.ok()) { await new Promise(r => setTimeout(r, 1000)); continue; }
            const health = await healthResp.json();
            const status = health.pipelines?.[livePipelineId]?.input?.status;
            if (status === 'on') break;
            await new Promise(r => setTimeout(r, 1000));
        }

        await ctx.dispose();
    });

    test.afterAll(async () => {
        if (ffmproc) {
            ffmproc.kill('SIGTERM');
            try {
                await new Promise<void>((resolve, reject) => {
                    ffmproc!.on('exit', () => resolve());
                    setTimeout(() => reject(new Error('timeout')), 5000);
                });
            } catch { /* ignore */ }
            ffmproc = null;
        }
        if (livePipelineId) {
            const ctx = await request.newContext({ baseURL: TEST_BASE_URL });
            await ctx.post('/api/v1/auth/login', { data: { password: 'admin' } });
            await ctx.delete(`/api/v1/pipelines/${livePipelineId}`).catch(() => {});
            await ctx.dispose();
        }
    });

    test.beforeEach(async ({ page }) => {
        await loginToLegacyDashboard(page);
    });

    test('HLS playlist is served for active pipeline', async ({ page }) => {
        // First request triggers segmenter start — may return 404 "No segments yet".
        // Retry until the segmenter produces its first playlist.
        const maxAttempts = 20;
        let lastBody = '';
        for (let attempt = 1; attempt <= maxAttempts; attempt++) {
            const resp = await page.request.get(`/hls/${livePipelineId}/index.m3u8`);
            if (resp.ok()) {
                lastBody = await resp.text();
                if (lastBody.includes('#EXTINF') && lastBody.includes('seg')) {
                    expect(resp.ok()).toBe(true);
                    expect(lastBody).toContain('#EXTM3U');
                    expect(lastBody).toContain('#EXTINF');
                    expect(lastBody).toContain('seg');
                    return;
                }
            }
            await page.waitForTimeout(1000);
        }
        // Final attempt for assertion failure message
        const finalResp = await page.request.get(`/hls/${livePipelineId}/index.m3u8`);
        expect(finalResp.ok()).toBe(true);
        const finalBody = await finalResp.text();
        expect(finalBody).toContain('#EXTM3U');
        expect(finalBody).toContain('#EXTINF');
        expect(finalBody).toContain('seg');
    });

    test('HLS segment can be downloaded', async ({ page }) => {
        // Wait for a playlist with segments
        let playlist = '';
        for (let attempt = 1; attempt <= 20; attempt++) {
            const resp = await page.request.get(`/hls/${livePipelineId}/index.m3u8`);
            if (resp.ok()) {
                playlist = await resp.text();
                if (playlist.includes('seg')) break;
            }
            await page.waitForTimeout(1000);
        }
        const segMatch = playlist.match(/^(seg\d+\.m4s)$/m);
        expect(segMatch).not.toBeNull();
        const segName = segMatch![1];

        const segResp = await page.request.get(`/hls/${livePipelineId}/${segName}`);
        expect(segResp.ok()).toBe(true);
        const segBytes = await segResp.body();
        expect(segBytes.length).toBeGreaterThan(1000);
    });

    test('HLS segmenter auto-started on first playlist request', async ({ page }) => {
        const pipeKey = `autotest_${Date.now()}`;
        const createResp = await page.request.post('/api/v1/pipelines', {
            data: { name: 'AutoStartTest', streamKey: pipeKey },
            headers: { 'Content-Type': 'application/json' },
        });
        expect(createResp.ok()).toBe(true);
        const createJson = await createResp.json();
        const pipeId = createJson.pipeline.id;

        const healthBefore = await page.request.get('/api/v1/engine/health');
        const healthJson = await healthBefore.json();
        expect(healthJson.pipelines[pipeId].hlsPreview.active).toBe(false);

        await page.request.delete(`/api/v1/pipelines/${pipeId}`);
    });

    test('ui=v1 (legacy): select pipeline and click Play preview triggers HLS load', async ({ page }) => {
        await openPipelineWorkspace(page);
        const pipelineItem = page.locator('#pipelines li', {
            hasText: livePipelineName,
        });
        await expect(pipelineItem).toBeVisible({ timeout: 10000 });
        await pipelineItem.click();

        const pipeInfoCol = page.locator('#pipe-info-col');
        await expect(pipeInfoCol).toBeVisible();

        const videoPlayer = page.locator('#video-player');
        await expect(videoPlayer).toBeVisible();

        const video = videoPlayer.locator('video[data-role="input-preview-video"]');
        await expect(video).toBeAttached();

        const playBtn = videoPlayer.locator('button', { hasText: 'Play preview' });
        await expect(playBtn).toBeVisible();
        await playBtn.click();

        const usesHlsJs = await page.evaluate(() => !!window.Hls);
        if (usesHlsJs) {
            // Wait for Hls.js to attach media and set src to a blob URL (since manifest load is async)
            await expect(video).toHaveAttribute('src', /blob:/, { timeout: 15000 });
            const vidPreviewSrc = await video.getAttribute('data-preview-src');
            expect(vidPreviewSrc).toContain(`/hls/${livePipelineId}/master.m3u8`);
        } else {
            // Wait for native player to set src to the HLS URL
            await expect(video).toHaveAttribute('src', new RegExp(`/hls/${livePipelineId}/master.m3u8`), { timeout: 15000 });
        }
    });

    test('ui=v2: mounts the complete HLS player in React and loads preview media', async ({ page }) => {
        const standbyPipelineName = `PlaywrightHlsStandby_${Date.now()}`;
        const standbyResponse = await page.request.post('/api/v1/pipelines', {
            data: {
                name: standbyPipelineName,
                streamKey: `pw_hls_standby_${Date.now()}`,
            },
        });
        expect(standbyResponse.ok()).toBe(true);
        const standbyPipelineId = (await standbyResponse.json()).pipeline.id;
        await page.goto('/?mode=pipeline&ui=v2');

        const pipelineSelector = page.locator('#dashboard-v2-pipeline-selector-root');
        await selectPipelineInV2Selector(
            pipelineSelector,
            livePipelineId,
            livePipelineName,
        );

        const inputStatus = page.locator('#dashboard-v2-pipeline-input-status-root');
        const previewPlayer = inputStatus.locator(
            '[data-role="dashboard-v2-input-preview"]',
        );
        await expect(previewPlayer).toBeVisible();
        await expect(page.locator('#video-player')).toBeHidden();

        const video = previewPlayer.locator(
            'video[data-role="input-preview-video"]',
        );
        await expect(video).toBeAttached();
        await previewPlayer
            .getByRole('button', { name: /Play (input )?preview/ })
            .click();

        if (await page.evaluate(() => !!window.Hls)) {
            await expect(video).toHaveAttribute('src', /blob:/, {
                timeout: 15000,
            });
        } else {
            await expect(video).toHaveAttribute(
                'src',
                new RegExp(`/hls/${livePipelineId}/master.m3u8`),
                { timeout: 15000 },
            );
        }
        await expect(video).toHaveAttribute(
            'data-preview-src',
            new RegExp(`/hls/${livePipelineId}/master.m3u8`),
        );
        await expectPreviewPlayback(video);

        const mountedVideo = await video.elementHandle();
        await selectPipelineInV2Selector(
            pipelineSelector,
            standbyPipelineId,
            standbyPipelineName,
        );
        await expect(previewPlayer).toHaveCount(0);
        expect(
            await mountedVideo?.evaluate(
                (element) => element.dataset.previewDisposed,
            ),
        ).toBe('true');

        await page.request.delete(`/api/v1/pipelines/${standbyPipelineId}`);
    });

    test('video starts playback after clicking Play preview', async ({ page }) => {
        await openPipelineWorkspace(page);
        const pipelineItem = page.locator('#pipelines li', {
            hasText: livePipelineName,
        });
        await pipelineItem.click();

        const playBtn = page.locator('#video-player button', { hasText: 'Play preview' });
        await expect(playBtn).toBeVisible({ timeout: 5000 });

        await playBtn.click();

        const video = page.locator('video[data-role="input-preview-video"]');
        await expect(video).toBeAttached();
        await expectPreviewPlayback(video);
    });

    test('HLS playlist advances media sequence while streaming', async ({ page }) => {
        const getSeq = async (): Promise<number> => {
            for (let attempt = 1; attempt <= 20; attempt++) {
                const resp = await page.request.get(`/hls/${livePipelineId}/index.m3u8`);
                if (resp.ok()) {
                    const body = await resp.text();
                    const matches = [...body.matchAll(/seg(\d+)\.m4s/g)];
                    if (matches.length > 0) {
                        const indexes = matches.map(m => parseInt(m[1], 10));
                        return Math.max(...indexes);
                    }
                }
                await page.waitForTimeout(1000);
            }
            return -1;
        };

        const seq1 = await getSeq();
        expect(seq1).toBeGreaterThanOrEqual(0);

        // Segments are ~6s each, so poll up to 12s for the next segment
        let seq2 = seq1;
        for (let attempt = 1; attempt <= 12; attempt++) {
            await page.waitForTimeout(1000);
            seq2 = await getSeq();
            if (seq2 > seq1) break;
        }
        expect(seq2).toBeGreaterThan(seq1);
    });

    test('egress output lifecycle start and stop', async ({ page }) => {
        const ctx = await request.newContext({ baseURL: TEST_BASE_URL });
        await ctx.post('/api/v1/auth/login', { data: { password: 'admin' } });

        const outputResp = await ctx.post(`/api/v1/pipelines/${livePipelineId}/outputs`, {
            data: {
                name: 'E2E-Egress-Test',
                url: 'rtmp://127.0.0.1:11935/live/e2e_egress_out',
                config: {
                    video: { mode: 'source' },
                    audio: { mode: 'all' },
                },
            }
        });
        expect(outputResp.ok()).toBe(true);
        const outputJson = await outputResp.json();
        const outputId = outputJson.output.id;
        await ctx.dispose();

        await page.goto('/');
        await openPipelineWorkspace(page);
        const pipelineItem = page.locator('#pipelines li', { hasText: livePipelineName });
        await pipelineItem.click();

        const outputRow = page.locator('#outputs-list > div', { hasText: 'E2E-Egress-Test' }).first();
        const startBtn = outputRow.locator('button[data-action="toggle-output"]', { hasText: 'Start' });
        await expect(startBtn).toBeVisible({ timeout: 10000 });
        await startBtn.click();

        const stopBtn = outputRow.locator('button[data-action="toggle-output"]', { hasText: 'Stop' });
        await expect(stopBtn).toBeVisible({ timeout: 15000 });

        const delCtx = await request.newContext({ baseURL: TEST_BASE_URL });
        await delCtx.post('/api/v1/auth/login', { data: { password: 'admin' } });
        await delCtx.delete(`/api/v1/pipelines/${livePipelineId}/outputs/${outputId}`);
        await delCtx.dispose();
    });

    test('settings persistence validation', async ({ page }) => {
        await openPipelineWorkspace(page);
        const pipelineItem = page.locator('#pipelines li', { hasText: livePipelineName });
        await expect(pipelineItem).toBeVisible({ timeout: 10000 });

        await page.locator('#workspace-tab-settings').click();
        const serverNameInput = page.locator('#settings-server-name');
        await expect(serverNameInput).toBeVisible();

        const originalName = await serverNameInput.inputValue();
        expect(originalName).not.toBe('');

        const newName = `TestRestream_${Date.now()}`;
        await serverNameInput.fill(newName);
        await page.locator('button[data-settings-action="save-server-name"]').click();

        await expect(page.locator('#server-name-saved')).toBeVisible();

        await page.goto('/');
        await expect(page.locator('button', { hasText: `Restream: ${newName}` })).toBeVisible({ timeout: 10000 });
        await page.locator('#workspace-tab-settings').click();
        await expect(serverNameInput).toHaveValue(newName);

        await serverNameInput.fill(originalName);
        await page.locator('button[data-settings-action="save-server-name"]').click();
        await expect(page.locator('#server-name-saved')).toBeVisible();
    });

    test('diagnostics modal auditing', async ({ page }) => {
        await selectPipelineForWorkspace(page, livePipelineName);
        await page.locator('#pipeline-workspace-tab-inspect').click();

        const select = page.locator('#inspect-pipeline-select');
        await expect(select).toBeVisible();
        await select.selectOption({ value: livePipelineId });

        const runDiagBtn = page.locator('#inspect-open-diagnostics-btn');
        await expect(runDiagBtn).toBeVisible();
        await expect(runDiagBtn).toBeEnabled();
        await runDiagBtn.click();

        const modal = page.locator('#diagnostics-modal');
        await expect(modal).toBeVisible();
        await expect(modal.locator('#diagnostics-title')).toContainText('Diagnostics');

        await expect(modal.locator('text=GOP Analysis')).toBeVisible({ timeout: 10000 });
        await expect(modal.locator('text=System Resources')).toBeVisible();

        const closeBtn = modal.locator('button', { hasText: 'Close' }).first();
        await expect(closeBtn).toBeVisible();
        await closeBtn.click();

        await expect(modal).not.toBeVisible();
    });

    test('Control Room input HLS preview player verification', async ({ page }) => {
        await selectPipelineForWorkspace(page, livePipelineName);
        await page.locator('#pipeline-workspace-tab-monitor').click();

        const select = page.locator('#control-room-pipeline-select');
        await expect(select).toBeVisible();
        await select.selectOption({ value: livePipelineId });

        const inputCard = page.locator('article[data-card-id^="input:"]').filter({
            has: page.getByRole('heading', { name: 'Primary', exact: true }),
        });
        await expect(inputCard).toBeVisible();
        await inputCard
            .getByRole('button', { name: 'Load preview for Primary' })
            .click();

        const managedVideo = inputCard.locator('video[data-role="managed-hls-video"]');
        await expect(managedVideo).toBeAttached({ timeout: 10000 });
        await expectPreviewPlayback(managedVideo);
    });
});

test.describe.serial('HLS Player — alternate audio preview', () => {
    let pipelineId: string;
    let pipelineName: string;

    test.beforeAll(async () => {
        const ctx = await request.newContext({ baseURL: TEST_BASE_URL });
        await ctx.post('/api/v1/auth/login', { data: { password: 'admin' } });

        pipelineName = `PlaywrightAltAudio_${Date.now()}`;
        const pipeKey = `pw_alt_audio_${Date.now()}`;
        const createResp = await ctx.post('/api/v1/pipelines', {
            data: { name: pipelineName, streamKey: pipeKey },
        });
        expect(createResp.ok()).toBe(true);
        const createJson = await createResp.json();
        pipelineId = createJson.pipeline.id;
        expect(pipelineId).toBeTruthy();

        const configureResp = await ctx.put(`/api/v1/pipelines/${pipelineId}/file-ingest`, {
            data: {
                filename: 'colorbar-timer-2v16a.mp4',
                loop: true,
                liveOptimized: true,
                targetGopSeconds: 4,
            },
        });
        expect(configureResp.ok()).toBe(true);

        const ingestResp = await ctx.get(`/api/v1/pipelines/${pipelineId}/file-ingest`);
        expect(ingestResp.ok()).toBe(true);
        const ingestJson = await ingestResp.json();
        const ingestId = ingestJson.id;
        expect(ingestId).toBeTruthy();

        const startResp = await ctx.post(`/api/v1/ingests/${ingestId}/start`);
        expect(startResp.ok()).toBe(true);

        await waitFor(
            async () => {
                const healthResp = await ctx.get('/api/v1/engine/health');
                return healthResp.ok() ? await healthResp.json() : null;
            },
            (health) => {
                const input = health?.pipelines?.[pipelineId]?.input;
                return input?.status === 'on' && (input?.audioTracks?.length || 0) >= 16;
            },
            40,
            1000,
        );

        await waitFor(
            async () => {
                const playlistResp = await ctx.get(`/hls/${pipelineId}/master.m3u8`);
                return playlistResp.ok() ? await playlistResp.text() : '';
            },
            (body) =>
                body.includes('video/index.m3u8') &&
                body.includes('audio/15/index.m3u8') &&
                (body.match(/#EXT-X-MEDIA:TYPE=AUDIO/g) || []).length >= 16,
            40,
            500,
        );

        await ctx.dispose();
    });

    test.afterAll(async () => {
        if (!pipelineId) return;
        const ctx = await request.newContext({ baseURL: TEST_BASE_URL });
        await ctx.post('/api/v1/auth/login', { data: { password: 'admin' } });
        await ctx.delete(`/api/v1/pipelines/${pipelineId}`).catch(() => {});
        await ctx.dispose();
    });

    test.beforeEach(async ({ page }) => {
        await loginToLegacyDashboard(page);
    });

    test('master playlist advertises alternate audio renditions', async ({ page }) => {
        const response = await page.request.get(`/hls/${pipelineId}/master.m3u8`);
        expect(response.ok()).toBe(true);
        const playlist = await response.text();

        expect(playlist).toContain('video/index.m3u8');
        expect(playlist).toContain('audio/0/index.m3u8');
        expect(playlist).toContain('audio/15/index.m3u8');
        expect((playlist.match(/#EXT-X-MEDIA:TYPE=AUDIO/g) || []).length).toBe(16);
    });

    test('ui=v1 (legacy): browser preview loads video and switches alternate audio tracks', async ({ page }) => {
        await openPipelineWorkspace(page);
        const pipelineItem = page.locator('#pipelines li', { hasText: pipelineName });
        await expect(pipelineItem).toBeVisible({ timeout: 10000 });
        await pipelineItem.click();

        const videoPlayer = page.locator('#video-player');
        await verifyAlternateAudioTrackSwitch(page, videoPlayer, pipelineId);
    });

    test('ui=v2: browser preview loads video and switches alternate audio tracks', async ({ page }) => {
        await page.goto('/?mode=pipeline&ui=v2');
        const pipelineSelector = page.locator('#dashboard-v2-pipeline-selector-root');
        await selectPipelineInV2Selector(pipelineSelector, pipelineId, pipelineName);

        const inputStatus = page.locator('#dashboard-v2-pipeline-input-status-root');
        const previewPlayer = inputStatus.locator('[data-role="dashboard-v2-input-preview"]');
        await expect(previewPlayer).toBeVisible();

        await verifyAlternateAudioTrackSwitch(page, previewPlayer, pipelineId);
    });
});
