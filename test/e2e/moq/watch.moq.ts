// Plays each MoQ endpoint the harness started with @moq/watch for 10 s and
// checks real playback: the negotiated protocol is moq-lite-06 over
// WebTransport, decoded video frames advance and the canvas repaints, and
// audio is received and decoded. An endpoint with a `refuseUrl` (Restream's
// origin without its subscribe token) must refuse the page instead; the
// reason it gave is recorded.
//
// Firefox only offers WebTransport protocols (and so moq-lite-06) from 155.
// An older Firefox is recorded as a fallback result (the version and
// transport it negotiated, and what played) and the test is skipped, so it
// never counts as a moq-lite-06 pass.
import { expect, test, type Page } from '@playwright/test';

interface Target {
    id: string;
    // The endpoint; for Restream's origin the bare listener, no token.
    url: string;
    name: string;
    // Restream's origin: the API route minting a subscribe grant
    // (`{token, url, broadcast, expiresAt}`).
    tokenPath?: string;
}

interface Subscription {
    url: string;
    name: string;
}

interface PageState {
    version?: string;
    transport?: string;
    error?: string;
    videoFrames: number;
    audioBytes: number;
    audioTimestampMs?: number;
    audioSampleRate?: number;
    pictureChanges: number;
    lumaMin?: number;
    lumaMax?: number;
}

const targets: Target[] = JSON.parse(process.env.MOQ_BROWSER_TARGETS ?? '[]');
const certificateSha256 = process.env.MOQ_BROWSER_CERT_SHA256 ?? '';
const api = process.env.MOQ_BROWSER_API ?? '';
const apiCookie = process.env.MOQ_BROWSER_API_COOKIE ?? '';
const WATCH_MS = 10_000;
// Half the fixture's 30 fps over the window: decoding keeps up with live.
const MIN_VIDEO_FRAMES = 150;
// The fixture (av-marker-h264-bf0.ts) is black with a white marker flashed
// every 5 s, so 10 s of playback paints at least one flash on and off.
const MIN_PICTURE_CHANGES = 2;
const MIN_LUMA_RANGE = 100;
const MIN_AUDIO_ADVANCE_MS = 5_000;
const FIRST_FIREFOX_WITH_PROTOCOLS = 155;

test.skip(targets.length === 0, 'run through `scripts/harness/run.sh moq-dev-interop -- --browsers`');

async function state(page: Page): Promise<PageState> {
    return page.evaluate(() => window.moqWatch.state());
}

// What to subscribe to. A grant's token admits for 60 s, so each test mints
// its own with the harness's API session; its path and query go onto the
// listener URL the harness bound (127.0.0.1), whatever host Restream
// advertises.
async function subscription(target: Target): Promise<Subscription> {
    if (!target.tokenPath) {
        return { url: target.url, name: target.name };
    }
    const response = await fetch(`${api}${target.tokenPath}`, {
        method: 'POST',
        headers: { cookie: apiCookie, 'content-type': 'application/json' },
        body: '{}',
    });
    expect(response.ok, `mint a subscribe token: HTTP ${response.status}`).toBe(true);
    const grant: { url: string; broadcast?: string } = await response.json();
    const advertised = new URL(grant.url);
    const url = new URL(target.url);
    url.pathname = advertised.pathname;
    url.search = advertised.search;
    return { url: url.toString(), name: grant.broadcast ?? target.name };
}

function pageUrl(subscribe: Subscription, browserName: string): string {
    const query = new URLSearchParams({
        url: subscribe.url,
        name: subscribe.name,
        hash: certificateSha256,
        // Chromium must win on WebTransport; Firefox may fall back.
        ws: browserName === 'firefox' ? '1' : '0',
    });
    return `/index.html?${query}`;
}

for (const target of targets.filter((candidate) => candidate.tokenPath)) {
    test(`refuses ${target.id} without a token`, async ({ page, browserName }) => {
        await page.goto(pageUrl({ url: target.url, name: target.name }, browserName));
        // Refused at the WebTransport handshake (connect rejects) or right
        // after it (the session closes): either way the page reports an error.
        await expect
            .poll(async () => (await state(page)).error ?? '', { timeout: 20_000 })
            .not.toBe('')
            .catch(() => undefined);
        await page.waitForTimeout(2_000);
        const refused = await state(page);
        test.info().annotations.push(
            { type: 'moq-refusal', description: refused.error ?? 'none' },
            { type: 'moq-protocol', description: `${refused.version ?? 'none'}/${refused.transport ?? 'none'}` },
        );
        expect(refused.error, 'refusal reason').toBeDefined();
        expect(refused.videoFrames, 'video decoded without a token').toBe(0);
        expect(refused.audioBytes, 'audio received without a token').toBe(0);
    });
}

for (const target of targets) {
    test(`plays ${target.id}`, async ({ page, browser, browserName }) => {
        const browserVersion = browser.version();
        await page.goto(pageUrl(await subscription(target), browserName));
        await expect
            .poll(async () => {
                const current = await state(page);
                return current.version ?? current.error ?? '';
            }, { timeout: 20_000 })
            .not.toBe('');
        // Start the window once both decoders produce, so the audio timeline
        // advance is measured from a real timestamp. A stream that never
        // starts runs into the window and fails its checks with the numbers.
        await expect
            .poll(async () => {
                const current = await state(page);
                return current.error !== undefined || (current.videoFrames > 0 && (current.audioTimestampMs ?? 0) > 0);
            }, { timeout: 15_000 })
            .toBe(true)
            .catch(() => undefined);

        await page.evaluate(() => window.moqWatch.markWindow());
        const first = await state(page);
        await page.waitForTimeout(WATCH_MS);
        const last = await state(page);
        const protocol = `${first.version ?? 'none'}/${first.transport ?? 'none'}`;
        const playback = {
            videoFrames: last.videoFrames - first.videoFrames,
            pictureChanges: last.pictureChanges,
            lumaRange: (last.lumaMax ?? 0) - (last.lumaMin ?? 0),
            audioBytes: last.audioBytes - first.audioBytes,
            audioAdvanceMs: (last.audioTimestampMs ?? 0) - (first.audioTimestampMs ?? 0),
            audioSampleRate: last.audioSampleRate ?? null,
            error: last.error ?? null,
        };
        test.info().annotations.push(
            { type: 'moq-browser-version', description: browserVersion },
            { type: 'moq-protocol', description: protocol },
            { type: 'moq-playback', description: JSON.stringify(playback) },
        );

        const major = Number.parseInt(browserVersion, 10);
        test.skip(
            browserName === 'firefox' && major < FIRST_FIREFOX_WITH_PROTOCOLS,
            `fallback result: Firefox ${browserVersion} negotiated ${protocol}`,
        );

        expect(playback.error, 'page error').toBeNull();
        expect(protocol, 'negotiated protocol').toBe('moq-lite-06/webtransport');
        expect(playback.videoFrames, 'decoded video frames in 10 s').toBeGreaterThanOrEqual(MIN_VIDEO_FRAMES);
        expect(playback.pictureChanges, 'canvas picture changes in 10 s').toBeGreaterThanOrEqual(MIN_PICTURE_CHANGES);
        expect(playback.lumaRange, 'canvas luma range in 10 s').toBeGreaterThanOrEqual(MIN_LUMA_RANGE);
        expect(playback.audioBytes, 'audio bytes received in 10 s').toBeGreaterThan(0);
        expect(playback.audioSampleRate ?? 0, 'decoded audio sample rate').toBeGreaterThan(0);
        expect(playback.audioAdvanceMs, 'decoded audio timeline advance').toBeGreaterThanOrEqual(MIN_AUDIO_ADVANCE_MS);
    });
}
