'use strict';

// MediaMTX client utilities: base URLs, JSON fetcher, and reader-tag helpers.
// All constants are derived from the fixed localhost binding that MediaMTX uses in this
// deployment. Any module that talks to MediaMTX can require this directly instead of
// receiving these helpers through the DI parameter list in index.js.

const { errMsg } = require('./app');

const fetch = global.fetch || require('node-fetch');

// MediaMTX API and RTSP are always on localhost with hardcoded ports.
const MEDIAMTX_API_BASE = 'http://localhost:9997';
const MEDIAMTX_RTSP_BASE = 'rtsp://localhost:8554';
const MEDIAMTX_FETCH_TIMEOUT_MS = 5000;

function getMediamtxApiBaseUrl() {
    return MEDIAMTX_API_BASE;
}

function getMediamtxRtspBaseUrl() {
    return MEDIAMTX_RTSP_BASE;
}

async function fetchMediamtxJson(endpoint) {
    const url = `${MEDIAMTX_API_BASE}${endpoint}`;
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

// ── Reader-tag helpers ────────────────────────────────
// FFmpeg output jobs embed a reader_id query param in the RTSP URL so the health collector
// can correlate live RTSP connections back to specific pipeline+output pairs.

function generateReaderTag(pipelineId, outputId) {
    return `reader_${pipelineId}_${outputId}`.replace(/[^a-zA-Z0-9_-]/g, '_');
}

function getPipelineTaggedRtspUrl(streamKey, pipelineId, outputId) {
    const readerTag = generateReaderTag(pipelineId, outputId);
    return `${MEDIAMTX_RTSP_BASE}/${streamKey}?reader_id=${encodeURIComponent(readerTag)}`;
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
        return params.get('reader_id') || null;
    } catch {
        return null;
    }
}

module.exports = {
    MEDIAMTX_FETCH_TIMEOUT_MS,
    getMediamtxApiBaseUrl,
    getMediamtxRtspBaseUrl,
    fetchMediamtxJson,
    generateReaderTag,
    getPipelineTaggedRtspUrl,
    getExpectedReaderTag,
    getReaderIdFromQuery,
};
