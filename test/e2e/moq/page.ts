// Minimal @moq/watch player for the MoQ browser interop specs.
//
// Query: `url` (https:// MoQ endpoint), `name` (broadcast path), `hash` (hex
// SHA-256 of the endpoint's ECDSA leaf, pinned via serverCertificateHashes)
// and `ws=1` to allow @moq/net's WebSocket fallback. `window.moqWatch` exposes
// what the specs sample: the negotiated version and transport, decoder
// progress, and what the canvas actually showed.
import * as Watch from '@moq/watch';

interface PageState {
    version?: string;
    transport?: string;
    error?: string;
    videoFrames: number;
    videoBytes: number;
    videoTimestampMs?: number;
    audioBytes: number;
    audioTimestampMs?: number;
    audioSampleRate?: number;
    // Since the last markWindow(): painted pictures that differ from the one
    // before, and the darkest and brightest mean luma painted.
    pictureChanges: number;
    lumaMin?: number;
    lumaMax?: number;
}

declare global {
    interface Window {
        moqWatch: {
            state(): PageState;
            markWindow(): void;
        };
    }
}

const params = new URLSearchParams(location.search);
const canvas = document.getElementById('video') as HTMLCanvasElement;
const fixed: Pick<PageState, 'version' | 'transport' | 'error'> = {};
let player: Watch.Player | undefined;

function required(key: string): string {
    const value = params.get(key);
    if (!value) {
        throw new Error(`missing query parameter ${key}`);
    }
    return value;
}

function fail(error: unknown): void {
    fixed.error ??= error instanceof Error ? `${error.name}: ${error.message}` : String(error);
}

window.addEventListener('error', (event) => fail(event.error ?? event.message));
window.addEventListener('unhandledrejection', (event) => fail(event.reason));

// Every animation frame, shrink the painted canvas to 16x9 and compare it with
// the previous one. The interop fixture is black with a white marker flash
// every 5 s, so a 10 s window that plays shows both extremes; a frozen or
// unpainted canvas shows neither change nor contrast.
const thumb = document.createElement('canvas');
thumb.width = 16;
thumb.height = 9;
const thumbContext = thumb.getContext('2d', { willReadFrequently: true });
const pictures = { changes: 0, lumaMin: undefined as number | undefined, lumaMax: undefined as number | undefined };
let previous = '';

function watchCanvas(): void {
    if (thumbContext && canvas.width > 0 && canvas.height > 0) {
        thumbContext.drawImage(canvas, 0, 0, thumb.width, thumb.height);
        const pixels = thumbContext.getImageData(0, 0, thumb.width, thumb.height).data;
        let luma = 0;
        for (let index = 0; index < pixels.length; index += 4) {
            luma += 0.299 * pixels[index] + 0.587 * pixels[index + 1] + 0.114 * pixels[index + 2];
        }
        luma /= pixels.length / 4;
        const picture = pixels.join(',');
        if (picture !== previous) {
            pictures.changes += 1;
            previous = picture;
        }
        pictures.lumaMin = Math.min(pictures.lumaMin ?? luma, luma);
        pictures.lumaMax = Math.max(pictures.lumaMax ?? luma, luma);
    }
    requestAnimationFrame(watchCanvas);
}
requestAnimationFrame(watchCanvas);

window.moqWatch = {
    state(): PageState {
        const video = player?.video.out.stats.peek();
        const audio = player?.audio.out.stats.peek();
        return {
            ...fixed,
            videoFrames: video?.frameCount ?? 0,
            videoBytes: video?.bytesReceived ?? 0,
            videoTimestampMs: player?.video.out.timestamp.peek(),
            audioBytes: audio?.bytesReceived ?? 0,
            audioTimestampMs: player?.audio.out.timestamp.peek(),
            audioSampleRate: player?.audio.out.sampleRate.peek(),
            pictureChanges: pictures.changes,
            lumaMin: pictures.lumaMin,
            lumaMax: pictures.lumaMax,
        };
    },
    markWindow(): void {
        pictures.changes = 0;
        pictures.lumaMin = undefined;
        pictures.lumaMax = undefined;
    },
};

async function start(): Promise<void> {
    const origin = new Watch.Net.Origin.Producer();
    // One-shot connect rather than the pooled Connection: only the
    // established session reports the negotiated version.
    const session = await Watch.Net.Connection.connect({
        url: new URL(required('url')),
        webtransport: {
            serverCertificateHashes: [{ algorithm: 'sha-256', value: required('hash') }],
        },
        websocket: { enabled: params.get('ws') === '1' },
        consume: origin,
    });
    fixed.version = session.version;
    fixed.transport = session.transport;
    void session.closed.then((error) => {
        if (error) {
            fail(error);
        }
    });
    player = new Watch.Player({
        origin,
        probe: session.probe,
        name: Watch.Net.Path.from(required('name')),
        canvas,
        muted: false,
        volume: 0.5,
        visible: 'always',
        // The harness only opens the page once the broadcast is live, so
        // subscribe directly instead of depending on announce discovery.
        announced: false,
    });
}

start().catch(fail);
