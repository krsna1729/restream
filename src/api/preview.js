'use strict';

const { Readable } = require('stream');

const STREAM_KEY_RE = /^[A-Za-z0-9_-]+$/;
const HLS_ASSET_SEGMENT_RE = /^[A-Za-z0-9._-]+$/;
const MAX_HLS_ASSET_PATH_CHARS = 512;
const MAX_HLS_ASSET_SEGMENTS = 16;
const HLS_PROXY_TIMEOUT_MS = 30000;
const MAX_HLS_MANIFEST_BYTES = 1024 * 1024;
const VIDEO_ONLY_MANIFEST_PATH = 'video-only.m3u8';

function parseHlsAssetPath(rawAssetPath) {
    const assetPath = typeof rawAssetPath === 'string' && rawAssetPath.trim()
        ? rawAssetPath.trim()
        : 'index.m3u8';

    if (assetPath.length > MAX_HLS_ASSET_PATH_CHARS) return null;

    const segments = assetPath.split('/');
    if (
        segments.length === 0 ||
        segments.length > MAX_HLS_ASSET_SEGMENTS ||
        segments.some(
            (segment) =>
                !segment ||
                segment === '.' ||
                segment === '..' ||
                !HLS_ASSET_SEGMENT_RE.test(segment),
        )
    ) {
        return null;
    }

    return {
        encodedPath: segments.map((segment) => encodeURIComponent(segment)).join('/'),
        rawPath: assetPath,
    };
}

function buildForwardRequestHeaders(req) {
    const headers = {};
    const ifNoneMatch = req.headers['if-none-match'];
    const ifModifiedSince = req.headers['if-modified-since'];
    const range = req.headers.range;

    if (typeof ifNoneMatch === 'string' && ifNoneMatch.trim()) {
        headers['if-none-match'] = ifNoneMatch;
    }
    if (typeof ifModifiedSince === 'string' && ifModifiedSince.trim()) {
        headers['if-modified-since'] = ifModifiedSince;
    }
    if (typeof range === 'string' && range.trim()) {
        headers.range = range;
    }

    return headers;
}

function copyAllowedUpstreamHeaders(upstreamResponse, res) {
    const passthroughHeaders = [
        'content-type',
        'cache-control',
        'etag',
        'last-modified',
        'accept-ranges',
        'content-range',
        'content-length',
    ];

    passthroughHeaders.forEach((headerName) => {
        const headerValue = upstreamResponse.headers.get(headerName);
        if (headerValue) res.setHeader(headerName, headerValue);
    });

    res.setHeader('x-content-type-options', 'nosniff');
}

function isManifestResponse(pathName, contentType) {
    return pathName.toLowerCase().endsWith('.m3u8') || /application\/(vnd\.apple\.mpegurl|x-mpegurl)/i.test(contentType || '');
}

function splitHlsAttributeList(attributeListText) {
    const attributes = [];
    let current = '';
    let inQuotes = false;

    for (const char of String(attributeListText || '')) {
        if (char === '"') inQuotes = !inQuotes;

        if (char === ',' && !inQuotes) {
            if (current.trim()) attributes.push(current.trim());
            current = '';
            continue;
        }

        current += char;
    }

    if (current.trim()) attributes.push(current.trim());
    return attributes;
}

function stripAudioAttributesFromStreamInf(streamInfLine) {
    const prefix = '#EXT-X-STREAM-INF:';
    if (typeof streamInfLine !== 'string' || !streamInfLine.startsWith(prefix)) {
        return streamInfLine;
    }

    const attributes = splitHlsAttributeList(streamInfLine.slice(prefix.length));
    const sanitizedAttributes = [];

    attributes.forEach((attribute) => {
        const separatorIndex = attribute.indexOf('=');
        if (separatorIndex === -1) {
            sanitizedAttributes.push(attribute);
            return;
        }

        const name = attribute.slice(0, separatorIndex).trim().toUpperCase();
        const value = attribute.slice(separatorIndex + 1).trim();

        if (name === 'AUDIO') return;

        if (name === 'CODECS') {
            const codecs = value
                .replace(/^"|"$/g, '')
                .split(',')
                .map((codec) => codec.trim())
                .filter(Boolean)
                .filter((codec) => !/^mp4a\./i.test(codec));

            if (codecs.length === 0) return;
            sanitizedAttributes.push(`CODECS="${codecs.join(',')}"`);
            return;
        }

        sanitizedAttributes.push(attribute);
    });

    return `${prefix}${sanitizedAttributes.join(',')}`;
}

function buildVideoOnlyManifest(masterManifestText) {
    const lines = String(masterManifestText || '').split(/\r?\n/);
    const preservedLines = [];
    let streamInfLine = null;
    let streamUri = null;

    for (let index = 0; index < lines.length; index += 1) {
        const line = lines[index].trim();
        if (!line) continue;

        if (line === '#EXTM3U') {
            preservedLines.push(line);
            continue;
        }

        if (line.startsWith('#EXT-X-VERSION:') || line === '#EXT-X-INDEPENDENT-SEGMENTS') {
            preservedLines.push(line);
            continue;
        }

        if (!streamInfLine && line.startsWith('#EXT-X-STREAM-INF:')) {
            streamInfLine = stripAudioAttributesFromStreamInf(line);
            for (let cursor = index + 1; cursor < lines.length; cursor += 1) {
                const nextLine = lines[cursor].trim();
                if (!nextLine || nextLine.startsWith('#')) continue;
                streamUri = nextLine;
                break;
            }
            break;
        }
    }

    if (!streamInfLine || !streamUri) return null;
    const parsedStreamUri = parseHlsAssetPath(streamUri);
    if (!parsedStreamUri) return null;

    const headerLines = preservedLines.length > 0 ? preservedLines : ['#EXTM3U'];
    return `${headerLines.join('\n')}\n${streamInfLine}\n${parsedStreamUri.rawPath}\n`;
}

async function fetchUpstreamAsset({
    req,
    res,
    fetch,
    log,
    streamKey,
    assetPath,
    query,
    getMediamtxHlsBaseUrl,
    buildMediamtxPath,
}) {
    const upstreamUrl = `${getMediamtxHlsBaseUrl()}/${buildMediamtxPath(streamKey)}/${assetPath}${query || ''}`;
    const abortController = new AbortController();
    const timeout = setTimeout(() => abortController.abort(), HLS_PROXY_TIMEOUT_MS);
    const abortOnClientClose = () => {
        if (!res.writableEnded) abortController.abort();
    };
    res.on('close', abortOnClientClose);

    try {
        return await fetch(upstreamUrl, {
            headers: buildForwardRequestHeaders(req),
            signal: abortController.signal,
        });
    } catch (err) {
        log('warn', 'HLS preview proxy upstream request failed', {
            streamKey,
            assetPath,
            error: err?.message || String(err),
        });
        return null;
    } finally {
        clearTimeout(timeout);
        res.off('close', abortOnClientClose);
    }
}

async function streamUpstreamResponse({ upstreamResponse, res, pathName }) {
    const contentType = upstreamResponse.headers.get('content-type') || '';
    if (isManifestResponse(pathName, contentType)) {
        const buffer = Buffer.from(await upstreamResponse.arrayBuffer());
        if (buffer.length > MAX_HLS_MANIFEST_BYTES) {
            res.removeHeader('content-type');
            return res.status(502).json({ error: 'Preview manifest exceeds safe proxy size limit' });
        }
        return res.send(buffer);
    }

    if (!upstreamResponse.body) {
        return res.end();
    }

    if (typeof upstreamResponse.body.pipe === 'function') {
        upstreamResponse.body.pipe(res);
        return;
    }

    Readable.fromWeb(upstreamResponse.body).pipe(res);
}

function registerPreviewProxyRoutes({ app, fetch, log, getMediamtxHlsBaseUrl, buildMediamtxPath }) {
    async function proxyHlsAsset(req, res, rawAssetPath) {
        const streamKey = String(req.params.streamKey || '').trim();
        if (!STREAM_KEY_RE.test(streamKey)) {
            return res.status(400).json({ error: 'Invalid stream key' });
        }

        let parsedAsset = parseHlsAssetPath(rawAssetPath);
        if (!parsedAsset) {
            return res.status(400).json({ error: 'Invalid HLS asset path' });
        }

        if (parsedAsset.rawPath === VIDEO_ONLY_MANIFEST_PATH) {
            const masterResponse = await fetchUpstreamAsset({
                req,
                res,
                fetch,
                log,
                streamKey,
                assetPath: 'index.m3u8',
                query: '',
                getMediamtxHlsBaseUrl,
                buildMediamtxPath,
            });

            if (!masterResponse) {
                return res.status(502).json({ error: 'Failed to fetch preview asset' });
            }

            if (!masterResponse.ok) {
                res.status(masterResponse.status);
                copyAllowedUpstreamHeaders(masterResponse, res);
                return streamUpstreamResponse({
                    upstreamResponse: masterResponse,
                    res,
                    pathName: 'index.m3u8',
                });
            }

            const masterBuffer = Buffer.from(await masterResponse.arrayBuffer());
            if (masterBuffer.length > MAX_HLS_MANIFEST_BYTES) {
                return res.status(502).json({ error: 'Preview manifest exceeds safe proxy size limit' });
            }

            const videoOnlyManifest = buildVideoOnlyManifest(masterBuffer.toString('utf8'));
            if (!videoOnlyManifest) {
                return res.status(502).json({ error: 'Failed to derive video-only preview manifest' });
            }

            copyAllowedUpstreamHeaders(masterResponse, res);
            res.removeHeader('content-length');
            return res.send(Buffer.from(videoOnlyManifest));
        }

        const query = req.originalUrl.includes('?')
            ? req.originalUrl.slice(req.originalUrl.indexOf('?'))
            : '';
        const upstreamResponse = await fetchUpstreamAsset({
            req,
            res,
            fetch,
            log,
            streamKey,
            assetPath: parsedAsset.encodedPath,
            query,
            getMediamtxHlsBaseUrl,
            buildMediamtxPath,
        });

        if (!upstreamResponse) {
            return res.status(502).json({ error: 'Failed to fetch preview asset' });
        }

        res.status(upstreamResponse.status);
        copyAllowedUpstreamHeaders(upstreamResponse, res);
        return streamUpstreamResponse({
            upstreamResponse,
            res,
            pathName: parsedAsset.rawPath,
        });
    }

    app.get('/preview/hls/:streamKey', async (req, res) => {
        await proxyHlsAsset(req, res, 'index.m3u8');
    });

    app.get('/preview/hls/:streamKey/*assetPath', async (req, res) => {
        const wildcard = req.params.assetPath;
        const assetPath = Array.isArray(wildcard)
            ? wildcard.join('/')
            : wildcard || 'index.m3u8';
        await proxyHlsAsset(req, res, assetPath);
    });
}

module.exports = {
    buildVideoOnlyManifest,
    registerPreviewProxyRoutes,
};
