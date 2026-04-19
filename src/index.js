/* top requires */
const express = require('express');
const compression = require('compression');
const fetch = global.fetch || require('node-fetch'); // keep compatibility
const db = require('./db');
const { getConfig, toPublicConfig } = require('./config');
const { registerConfigApi } = require('./api/config');
const { createHealthMonitorService } = require('./services/health');
const {
    buildFfmpegOutputArgs,
    createHttpError,
    normalizeOutputEncoding,
    registerOutputApi,
    validateOutputUrl,
} = require('./api/outputs');
const { createOutputLifecycleService } = require('./services/outputs');
const { registerPipelineApi } = require('./api/pipelines');
const { startServer } = require('./services/bootstrap');
const { registerSystemMetricsApi } = require('./api/metrics');

const app = express();
app.use(express.json());
app.use(
    compression({
        threshold: 1024,
        brotli: { enabled: true },
        filter: (req, res) => {
            if (req.headers['x-no-compression']) return false;
            const contentType = res.getHeader('Content-Type');
            if (typeof contentType === 'string' && contentType.includes('text/event-stream')) {
                return false;
            }
            return compression.filter(req, res);
        },
    }),
);

const { spawn } = require('child_process');
const path = require('path');
const crypto = require('crypto');
const { createHash } = crypto;

const processes = new Map(); // runtime only: jobId -> ChildProcess
const ffmpegCmd = process.env.FFMPEG_PATH || 'ffmpeg';
const ffprobeCmd = process.env.FFPROBE_PATH || 'ffprobe';
const appPort = Number(process.env.PORT || 3030);
const appHost = getConfig().host;
const logLevel = (process.env.LOG_LEVEL || 'info').toLowerCase();
const probeCacheTtlMs = Number(process.env.PROBE_CACHE_TTL_MS || 30000);
const healthSnapshotIntervalMs = Number(process.env.HEALTH_SNAPSHOT_INTERVAL_MS || 2000);

// ── Timing constants ──────────────────────────────────
const MEDIAMTX_CHECK_INTERVAL_MS = 5000;
const MEDIAMTX_FETCH_TIMEOUT_MS = 5000;
const FFPROBE_TIMEOUT_MS = 8000;
const JOB_STABILITY_CHECK_MS = 250;
const SIGKILL_ESCALATION_MS = 5000;
const MAX_NAME_LENGTH = 128;

// Runtime-only progress state from ffmpeg "-progress pipe:3" (never persisted to DB).
// NOTE: This is intentionally internal for now; a future API/WS endpoint can expose it.
const ffmpegProgressByJobId = new Map(); // jobId -> latest ffmpeg progress block
// Parsed output media info from FFmpeg stderr "Output #0" section.
// Set once when FFmpeg first reports output stream details; cleared on exit/error.
const ffmpegOutputMediaByJobId = new Map(); // jobId -> { video: {...}, audio: {...} }

function getOutputRecoveryConfig() {
    return getConfig().outputRecovery || {};
}

function getInputUnavailableExitGraceMs() {
    return Math.max(healthSnapshotIntervalMs * 3, 15000);
}

function getRetryDelayMs(failureCount) {
    const cfg = getOutputRecoveryConfig();
    const immediateRetries = Number(cfg.immediateRetries || 0);
    const immediateDelayMs = Number(cfg.immediateDelayMs || 1000);
    const backoffRetries = Number(cfg.backoffRetries || 0);
    const backoffBaseDelayMs = Number(cfg.backoffBaseDelayMs || 2000);
    const backoffMaxDelayMs = Number(cfg.backoffMaxDelayMs || backoffBaseDelayMs);
    const totalRetries = immediateRetries + backoffRetries;

    if (failureCount <= 0 || failureCount > totalRetries) {
        return null;
    }

    if (failureCount <= immediateRetries) {
        return immediateDelayMs;
    }

    const backoffAttempt = failureCount - immediateRetries;
    const multiplier = Math.pow(2, Math.max(0, backoffAttempt - 1));
    const delay = backoffBaseDelayMs * multiplier;
    return Math.min(delay, backoffMaxDelayMs);
}

const levelOrder = { error: 0, warn: 1, info: 2, debug: 3 };

function shouldLog(level) {
    const current = levelOrder[logLevel] ?? levelOrder.info;
    const target = levelOrder[level] ?? levelOrder.info;
    return target <= current;
}

function log(level, message, fields = {}) {
    if (!shouldLog(level)) return;
    const payload = {
        ts: new Date().toISOString(),
        level,
        message,
        ...fields,
    };
    // Keep logs single-line JSON to simplify grep and diff across runs.
    console.log(JSON.stringify(payload));
}

function shellQuote(arg) {
    const s = String(arg ?? '');
    if (/^[A-Za-z0-9_./:-]+$/.test(s)) return s;
    return `'${s.replace(/'/g, `'\\''`)}'`;
}

function buildCommandPreview(cmd, args) {
    return [cmd, ...(args || []).map(shellQuote)].join(' ');
}

function errMsg(err) {
    return (err && err.message) || String(err);
}

const { normalizeEtag, recomputeConfigEtag, recomputeEtag } = registerConfigApi({
    app,
    db,
    getConfig,
    toPublicConfig,
    createHash,
    errMsg,
});

function validateName(name, fieldLabel = 'Name') {
    if (typeof name !== 'string' || !name.trim()) {
        return `${fieldLabel} is required and must be a non-empty string`;
    }
    if (name.length > MAX_NAME_LENGTH) {
        return `${fieldLabel} must be ${MAX_NAME_LENGTH} characters or fewer`;
    }
    return null;
}

function maskToken(value) {
    const s = String(value ?? '');
    if (!s) return '';
    if (s.length <= 4) {
        if (s.length === 1) return s;
        return `${s[0]}...${s[s.length - 1]}`;
    }
    return `${s.slice(0, 2)}...${s.slice(-2)}`;
}

function redactSensitiveUrl(rawUrl) {
    if (!rawUrl || typeof rawUrl !== 'string') return rawUrl;

    let parsed;
    try {
        parsed = new URL(rawUrl);
    } catch {
        return maskToken(rawUrl);
    }

    if (parsed.username) parsed.username = '[REDACTED]';
    if (parsed.password) parsed.password = '[REDACTED]';

    const sensitiveParams =
        /key|streamkey|stream_key|token|secret|pass|passphrase|signature|sig|auth|streamid/i;
    for (const [paramKey] of parsed.searchParams.entries()) {
        if (sensitiveParams.test(paramKey)) {
            parsed.searchParams.set(paramKey, '[REDACTED]');
        }
    }

    const protocol = String(parsed.protocol || '').toLowerCase();
    if (['rtmp:', 'rtmps:', 'rtsp:', 'rtsps:', 'srt:'].includes(protocol)) {
        const segments = parsed.pathname.split('/');
        const lastIdx = segments.length - 1;
        if (lastIdx >= 1 && segments[lastIdx]) {
            segments[lastIdx] = maskToken(segments[lastIdx]);
            parsed.pathname = segments.join('/');
        }
    }

    parsed.hash = '';
    return parsed.toString();
}

function redactFfmpegArgs(args) {
    return (args || []).map((arg) => {
        const s = String(arg ?? '');
        return s.includes('://') ? redactSensitiveUrl(s) : s;
    });
}

function logPipelineConfigChanges(pipelineId, previousPipeline, nextPipeline) {
    if (!pipelineId || !previousPipeline || !nextPipeline) return;

    if (previousPipeline.name !== nextPipeline.name) {
        db.appendPipelineEvent(
            pipelineId,
            `[config] name changed from "${previousPipeline.name}" to "${nextPipeline.name}"`,
            'pipeline_config',
        );
    }

    if (previousPipeline.encoding !== nextPipeline.encoding) {
        db.appendPipelineEvent(
            pipelineId,
            `[config] encoding changed from ${previousPipeline.encoding || 'null'} to ${nextPipeline.encoding || 'null'}`,
            'pipeline_config',
        );
    }

    if (previousPipeline.streamKey !== nextPipeline.streamKey) {
        db.appendPipelineEvent(
            pipelineId,
            `[config] stream_key changed from ${previousPipeline.streamKey ? maskToken(previousPipeline.streamKey) : 'unassigned'} to ${nextPipeline.streamKey ? maskToken(nextPipeline.streamKey) : 'unassigned'}`,
            'pipeline_config',
        );
    }
}

function getMediamtxApiBaseUrl() {
    // MediaMTX internal API is always available on localhost:9997
    return 'http://localhost:9997';
}

function getMediamtxRtspBaseUrl() {
    // MediaMTX RTSP input is always available on localhost:8554
    return 'rtsp://localhost:8554';
}

// Parse FFmpeg's "Output #0" stderr section to extract actual output stream media info.
// FFmpeg prints these lines before encoding starts; we capture them once and discard the buffer.
// Example lines:
//   Stream #0:0: Video: h264 (libx264), yuv420p, 1280x720, q=-1--1, 3000 kb/s, 30 fps, 1k tbn
//   Stream #0:1: Audio: aac, 48000 Hz, stereo, fltp, 128 kb/s
// Returns { video: {...}, audio: {...} } once both are found, or null if not yet complete.
function tryParseOutputMedia(stderrText) {
    // Only look at the region after "Output #0" to avoid capturing input stream info.
    const outputSectionIdx = stderrText.indexOf('Output #0');
    if (outputSectionIdx === -1) return null;
    const outputSection = stderrText.slice(outputSectionIdx);

    let video = null;
    let audio = null;

    // Each output stream line starts with "    Stream #<n>:<m>" (possibly with lang tag "(eng)").
    // We scan all Stream lines in the output section.
    const streamLineRe = /Stream #\d+:\d+(?:\([^)]*\))?: (Video|Audio): (.+)/g;
    let m;
    while ((m = streamLineRe.exec(outputSection)) !== null) {
        const type = m[1];
        const rest = m[2];
        if (type === 'Video' && !video) {
            // e.g. "h264 (libx264) (avc1 / 0x31637661), yuv420p, 1280x720, q=-1--1, 3000 kb/s, 30 fps, 1k tbn"
            const codecMatch = rest.match(/^(\w+)/);
            // Anchor to pixel-format token (yuv420p, nv12, p010, gray, rgb*, bgr*) to avoid
            // matching the RTMP/FLV hex codec tag "0x31637661" that appears earlier in the line.
            const dimMatch = rest.match(
                /\b(?:yuv|nv|p0|gray|rgb|bgr)\w*(?:\([^)]*\))?,\s*(\d+)x(\d+)/i,
            );
            const fpsMatch = rest.match(/[\s,](\d+(?:\.\d+)?)\s*fps/);
            video = {
                codec: codecMatch ? codecMatch[1].toLowerCase() : null,
                width: dimMatch ? Number(dimMatch[1]) : null,
                height: dimMatch ? Number(dimMatch[2]) : null,
                fps: fpsMatch ? Number(fpsMatch[1]) : null,
                profile: null,
                level: null,
            };
        } else if (type === 'Audio' && !audio) {
            // e.g. "aac, 48000 Hz, stereo, fltp, 128 kb/s"
            const codecMatch = rest.match(/^(\w+)/);
            const rateMatch = rest.match(/(\d+)\s*Hz/);
            const chMatch = rest.match(/\b(stereo|mono|5\.1|7\.1|quadraphonic)\b/i);
            const chNumMatch = rest.match(/\b(\d+)\s*channels?\b/i);
            let channels = null;
            if (chMatch) {
                const ch = chMatch[1].toLowerCase();
                if (ch === 'stereo') channels = 2;
                else if (ch === 'mono') channels = 1;
                else if (ch === '5.1') channels = 6;
                else if (ch === '7.1') channels = 8;
                else if (ch === 'quadraphonic') channels = 4;
            } else if (chNumMatch) {
                channels = Number(chNumMatch[1]);
            }
            audio = {
                codec: codecMatch ? codecMatch[1].toLowerCase() : null,
                sample_rate: rateMatch ? Number(rateMatch[1]) : null,
                channels,
            };
        }
    }

    // Only return once we have at least video info (audio may be absent for video-only streams).
    if (!video) return null;
    return { video, audio };
}

function generateReaderTag(pipelineId, outputId) {
    return `reader_${pipelineId}_${outputId}`.replace(/[^a-zA-Z0-9_-]/g, '_');
}

function getPipelineTaggedRtspUrl(streamKey, pipelineId, outputId) {
    const readerTag = generateReaderTag(pipelineId, outputId);
    return `${getMediamtxRtspBaseUrl()}/${streamKey}?reader_id=${encodeURIComponent(readerTag)}`;
}

function getExpectedReaderTag(pipelineId, outputId) {
    return generateReaderTag(pipelineId, outputId);
}

function getReaderIdFromQuery(query) {
    if (!query || typeof query !== 'string') return null;
    const normalized = query.startsWith('?') ? query.slice(1) : query;
    if (!normalized) return null;
    try {
        const params = new URLSearchParams(normalized);
        const readerId = params.get('reader_id');
        return readerId || null;
    } catch (err) {
        return null;
    }
}

async function fetchMediamtxJson(endpoint) {
    const url = `${getMediamtxApiBaseUrl()}${endpoint}`;
    const resp = await fetch(url, {
        signal: AbortSignal.timeout(MEDIAMTX_FETCH_TIMEOUT_MS),
    });
    let data = null;
    try {
        data = await resp.json();
    } catch (err) {
        throw new Error(`Invalid JSON from MediaMTX endpoint ${endpoint}: ${errMsg(err)}`);
    }
    if (!resp.ok) {
        throw new Error(`MediaMTX ${endpoint} failed with status ${resp.status}`);
    }
    return data;
}

let outputLifecycle;
const healthMonitor = createHealthMonitorService({
    db,
    log,
    errMsg,
    fetch,
    fetchMediamtxJson,
    createHash,
    normalizeEtag,
    getMediamtxApiBaseUrl,
    getMediamtxRtspBaseUrl,
    getExpectedReaderTag,
    getReaderIdFromQuery,
    ffmpegProgressByJobId,
    ffmpegOutputMediaByJobId,
    normalizeOutputEncoding,
    restartPipelineOutputsOnInputRecovery: (pipelineId) =>
        outputLifecycle?.restartPipelineOutputsOnInputRecovery(pipelineId),
    getInputUnavailableExitGraceMs,
    mediamtxCheckIntervalMs: MEDIAMTX_CHECK_INTERVAL_MS,
    mediamtxFetchTimeoutMs: MEDIAMTX_FETCH_TIMEOUT_MS,
    probeCacheTtlMs,
    ffprobeTimeoutMs: FFPROBE_TIMEOUT_MS,
    healthSnapshotIntervalMs,
    ffprobeCmd,
    spawn,
});

outputLifecycle = createOutputLifecycleService({
    db,
    getConfig,
    log,
    errMsg,
    fetchMediamtxJson,
    getPipelineTaggedRtspUrl,
    getExpectedReaderTag,
    validateOutputUrl,
    normalizeOutputEncoding,
    buildFfmpegOutputArgs,
    redactFfmpegArgs,
    redactSensitiveUrl,
    buildCommandPreview,
    ffmpegCmd,
    spawn,
    processes,
    ffmpegProgressByJobId,
    ffmpegOutputMediaByJobId,
    tryParseOutputMedia,
    recomputeEtag,
    createHttpError,
    getRetryDelayMs,
    getInputUnavailableExitGraceMs,
    isLatestJobLikelyInputUnavailableStop: healthMonitor.isLatestJobLikelyInputUnavailableStop,
    JOB_STABILITY_CHECK_MS,
    SIGKILL_ESCALATION_MS,
});

const {
    clearOutputRestartState,
    getOutputDesiredState,
    reconcileOutput,
    resetOutputFailureCount,
    setOutputDesiredState,
    stopRunningJob,
} = outputLifecycle;

registerPipelineApi({
    app,
    db,
    getConfig,
    fetch,
    crypto,
    errMsg,
    getMediamtxApiBaseUrl,
    healthMonitor,
    maskToken,
    logPipelineConfigChanges,
    resetOutputFailureCount,
    clearOutputRestartState,
    stopRunningJob,
    recomputeConfigEtag,
    recomputeEtag,
    validateName,
});

registerOutputApi({
    app,
    db,
    getConfig,
    errMsg,
    recomputeConfigEtag,
    recomputeEtag,
    clearOutputRestartState,
    getOutputDesiredState,
    reconcileOutput,
    resetOutputFailureCount,
    setOutputDesiredState,
    stopRunningJob,
    validateName,
});

healthMonitor.registerRoutes(app);
registerSystemMetricsApi({ app, errMsg });

/* ======================
 * Static UI & Server
 * ====================== */

app.use(
    '/',
    express.static(path.join(__dirname, '..', 'public'), {
        maxAge: '1h',
        etag: true,
        lastModified: true,
    }),
);

startServer({ app, healthMonitor, db, log, appPort, appHost }).catch((err) => {
    console.error('Fatal startup error:', err);
    process.exit(1);
});
