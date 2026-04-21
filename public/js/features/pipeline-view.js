import { formatCodecName, maskSecret, msToHHMMSS, sanitizeLogMessage } from '../core/utils.js';
import { setBadgeBitrateWithSubtleUnit, setBitrateWithSubtleUnit } from './metric-format.js';
import { state } from '../core/state.js';
import {
    getPublisherQualityAlerts,
    normalizePublisherProtocolLabel,
} from './publisher-quality.js';

const pipelineViewDependencies = {
    openPipelineHistoryModal: null,
    openPublisherQualityModal: null,
    isOutputToggleBusy: null,
    startOutBtn: null,
    stopOutBtn: null,
    openOutputHistoryModal: null,
    editOutBtn: null,
    deleteOutBtn: null,
};

const INPUT_PREVIEW_VIDEO_SELECTOR = '[data-role="input-preview-video"]';
const HLS_RUNTIME_URL = '/vendor/hls.min.js';

let hlsRuntimePromise = null;

function canUseNativeHls(video) {
    return Boolean(
        video?.canPlayType('application/vnd.apple.mpegurl') ||
            video?.canPlayType('application/x-mpegURL'),
    );
}

function destroyPreviewController(video) {
    if (!video?._previewHls) return;
    video._previewHls.destroy();
    delete video._previewHls;
}

function loadHlsRuntime() {
    if (globalThis.Hls) return Promise.resolve(globalThis.Hls);
    if (hlsRuntimePromise) return hlsRuntimePromise;

    hlsRuntimePromise = new Promise((resolve, reject) => {
        const existingScript = document.querySelector('script[data-role="hls-runtime"]');

        function handleLoad() {
            if (globalThis.Hls) {
                resolve(globalThis.Hls);
                return;
            }
            reject(new Error('hls.js loaded without exporting a global Hls object'));
        }

        function handleError() {
            reject(new Error('Failed to load hls.js runtime'));
        }

        if (existingScript) {
            existingScript.addEventListener('load', handleLoad, { once: true });
            existingScript.addEventListener('error', handleError, { once: true });
            return;
        }

        const script = document.createElement('script');
        script.src = HLS_RUNTIME_URL;
        script.async = true;
        script.dataset.role = 'hls-runtime';
        script.addEventListener('load', handleLoad, { once: true });
        script.addEventListener('error', handleError, { once: true });
        document.head.appendChild(script);
    }).catch((err) => {
        hlsRuntimePromise = null;
        throw err;
    });

    return hlsRuntimePromise;
}

function buildInputPreviewUrl(streamKey) {
    return `/preview/hls/${encodeURIComponent(streamKey)}/video-only.m3u8`;
}

function clearInputPreview(playerElem) {
    if (!playerElem) return;
    const existingVideo = playerElem.querySelector(INPUT_PREVIEW_VIDEO_SELECTOR);
    if (existingVideo) {
        existingVideo.dataset.previewDisposed = 'true';
        destroyPreviewController(existingVideo);
        existingVideo.pause();
        existingVideo.removeAttribute('src');
        existingVideo.load();
    }
    playerElem.replaceChildren();
    delete playerElem.dataset.previewSrc;
}

function setPreviewMessage(playerElem, message) {
    clearInputPreview(playerElem);
    const messageEl = document.createElement('p');
    messageEl.className = 'text-sm opacity-70 px-3 py-4';
    messageEl.textContent = message;
    playerElem.appendChild(messageEl);
}

function renderInputPreview(playerElem, pipe) {
    if (!playerElem) return;

    if (!pipe?.key) {
        setPreviewMessage(playerElem, 'Preview unavailable: stream key is not assigned.');
        return;
    }

    const previewSrc = buildInputPreviewUrl(pipe.key);
    if (playerElem.dataset.previewSrc === previewSrc) {
        return;
    }

    clearInputPreview(playerElem);

    const shell = document.createElement('div');
    shell.style.position = 'relative';
    shell.style.width = '100%';
    shell.style.overflow = 'hidden';
    shell.style.borderRadius = '0.75rem';
    shell.style.background = 'var(--fallback-b3, oklch(var(--b3)/1))';
    shell.style.aspectRatio = '16 / 9';

    const video = document.createElement('video');
    video.dataset.role = 'input-preview-video';
    video.style.width = '100%';
    video.style.height = '100%';
    video.style.display = 'block';
    video.style.objectFit = 'contain';
    video.style.background = 'var(--fallback-b3, oklch(var(--b3)/1))';
    video.controls = false;
    video.muted = true;
    video.playsInline = true;
    video.preload = 'none';
    video.dataset.previewSrc = previewSrc;
    video.dataset.previewLoaded = 'false';

    const overlay = document.createElement('div');
    overlay.style.position = 'absolute';
    overlay.style.inset = '0';
    overlay.style.display = 'flex';
    overlay.style.alignItems = 'center';
    overlay.style.justifyContent = 'center';
    overlay.style.background = 'rgba(20, 26, 40, 0.42)';

    const loadBtn = document.createElement('button');
    loadBtn.type = 'button';
    loadBtn.className = 'btn btn-sm btn-accent';
    loadBtn.textContent = 'Play preview';

    const spinner = document.createElement('span');
    spinner.style.cssText = 'display:none;width:2rem;height:2rem;border-radius:9999px;border:3px solid rgba(255,255,255,0.25);border-top-color:#fff;animation:spin 0.8s linear infinite';
    let previewStarted = false;

    function setOverlayVisible(isVisible) {
        overlay.style.display = isVisible ? 'flex' : 'none';
        overlay.setAttribute('aria-hidden', isVisible ? 'false' : 'true');
    }

    function setOverlayButtonState({ buttonText, buttonDisabled }) {
        loadBtn.textContent = buttonText;
        loadBtn.disabled = !!buttonDisabled;
        loadBtn.classList.toggle('btn-disabled', !!buttonDisabled);
        if (buttonDisabled) {
            loadBtn.style.display = 'none';
            spinner.style.display = 'block';
        } else {
            loadBtn.style.display = '';
            spinner.style.display = 'none';
        }
    }

    const attemptPlayback = () => {
        if (video.dataset.previewDisposed === 'true') return;
        const playPromise = video.play();
        if (!playPromise || typeof playPromise.then !== 'function') return;
        void playPromise.catch((err) => {
            if (video.dataset.previewDisposed === 'true') return;
            if (err?.name === 'AbortError') {
                video.addEventListener(
                    'canplay',
                    () => {
                        attemptPlayback();
                    },
                    { once: true },
                );
                return;
            }
            video.dataset.previewLoaded = 'false';
            setOverlayButtonState({ buttonText: 'Play preview', buttonDisabled: false });
        });
    };

    const resetPreviewLoadState = () => {
        if (video.dataset.previewDisposed === 'true') return;
        previewStarted = false;
        video.dataset.previewLoaded = 'false';
        video.controls = false;
        destroyPreviewController(video);
        video.removeAttribute('src');
        video.load();
        setOverlayVisible(true);
        setOverlayButtonState({ buttonText: 'Play preview', buttonDisabled: false });
    };

    const bindHlsController = async () => {
        let Hls = null;
        let hlsRuntimeError = null;

        try {
            Hls = await loadHlsRuntime();
        } catch (err) {
            hlsRuntimeError = err;
        }

        if (video.dataset.previewDisposed === 'true') return;

        if (Hls?.isSupported?.()) {
            const hls = new Hls({
                enableWorker: true,
                lowLatencyMode: false,
            });
            video._previewHls = hls;

            hls.on(Hls.Events.ERROR, (_event, data) => {
                if (video.dataset.previewDisposed === 'true') return;
                if (!data?.fatal) return;

                if (data.type === Hls.ErrorTypes.NETWORK_ERROR) {
                    hls.startLoad();
                    return;
                }

                if (data.type === Hls.ErrorTypes.MEDIA_ERROR) {
                    hls.recoverMediaError();
                    return;
                }

                resetPreviewLoadState();
            });

            hls.on(Hls.Events.MANIFEST_PARSED, () => {
                attemptPlayback();
            });

            hls.on(Hls.Events.MEDIA_ATTACHED, () => {
                hls.loadSource(previewSrc);
            });
            hls.attachMedia(video);
            return;
        }

        if (canUseNativeHls(video)) {
            video.src = previewSrc;
            video.load();
            attemptPlayback();
            return;
        }

        throw hlsRuntimeError || new Error('This browser does not support dashboard preview playback');
    };

    const primePreviewSource = async () => {
        if (video.dataset.previewLoaded === 'true') return;
        previewStarted = false;
        video.dataset.previewLoaded = 'true';
        video.controls = false;
        setOverlayVisible(true);
        setOverlayButtonState({ buttonText: 'Loading...', buttonDisabled: true });

        try {
            await bindHlsController();
        } catch (err) {
            console.warn('Preview playback failed to initialize', err);
            resetPreviewLoadState();
        }
    };

    loadBtn.addEventListener('click', primePreviewSource);
    video.addEventListener('timeupdate', () => {
        if (video.dataset.previewDisposed === 'true') return;
        previewStarted = true;
        video.controls = true;
        setOverlayVisible(false);
    }, { once: true });
    video.addEventListener('error', () => {
        if (video.dataset.previewDisposed === 'true') return;
        if (video._previewHls || previewStarted) return;
        resetPreviewLoadState();
    });

    overlay.appendChild(spinner);
    overlay.appendChild(loadBtn);
    shell.appendChild(video);
    shell.appendChild(overlay);
    playerElem.appendChild(shell);
    playerElem.dataset.previewSrc = previewSrc;
}

function setPipelineViewDependencies(dependencies) {
    Object.assign(pipelineViewDependencies, dependencies || {});
}

    function renderPipelineInfoColumn(selectedPipe) {
        if (!selectedPipe) {
            document.getElementById('pipe-info-col').classList.add('hidden');
            return;
        }

        document.getElementById('pipe-info-col').classList.remove('hidden');

        const pipe = state.pipelines.find((p) => p.id === selectedPipe);
        if (!pipe) {
            console.error('Pipeline not found:', selectedPipe);
            return;
        }

        document.getElementById('pipe-name').textContent = pipe.name;
        const historyBtn = document.getElementById('pipe-history-btn');
        if (historyBtn) {
            historyBtn.onclick = () => {
                pipelineViewDependencies.openPipelineHistoryModal?.(pipe.id, pipe.name);
            };
        }
        const inputTimeElem = document.getElementById('input-time');
        if (inputTimeElem) {
            inputTimeElem.classList.add('hidden');
            inputTimeElem.textContent = pipe.input.time === null ? '' : msToHHMMSS(pipe.input.time);
        }

        const deletePipeBtn = document.getElementById('delete-pipe-btn');
        if (pipe.outs.find((o) => o.status !== 'off')) {
            deletePipeBtn.classList.add('btn-disabled');
            deletePipeBtn.title = 'Stop all outputs before deleting the pipeline';
        } else {
            deletePipeBtn.classList.remove('btn-disabled');
            deletePipeBtn.title = '';
        }

        const streamKey = pipe.key || 'Unassigned';
        const maskedStreamKey = pipe.key ? maskSecret(pipe.key) : streamKey;

        document.getElementById('stream-key').textContent = maskedStreamKey;
        document.getElementById('stream-key').dataset.copy = pipe.key || '';

        const ingestUrls = pipe.ingestUrls || {};

        const setIngestUrlRow = (rowId, valueId, url) => {
            const row = document.getElementById(rowId);
            const valueEl = document.getElementById(valueId);
            if (!row || !valueEl) return false;

            const hasUrl = typeof url === 'string' && url.trim() !== '';
            row.classList.toggle('hidden', !hasUrl);
            valueEl.textContent = hasUrl ? maskSecret(url) : '';
            valueEl.dataset.copy = hasUrl ? url : '';
            return hasUrl;
        };

        const hasRtmpUrl = setIngestUrlRow('ingest-url-rtmp-row', 'rtmp-url', ingestUrls.rtmp);
        const hasRtspUrl = setIngestUrlRow('ingest-url-rtsp-row', 'rtsp-url', ingestUrls.rtsp);
        const hasSrtUrl = setIngestUrlRow('ingest-url-srt-row', 'srt-url', ingestUrls.srt);
        const ingestHeaderRow = document.getElementById('ingest-urls-header-row');
        if (ingestHeaderRow) {
            ingestHeaderRow.classList.toggle('hidden', !(hasRtmpUrl || hasRtspUrl || hasSrtUrl));
        }

        const playerElem = document.getElementById('video-player');
        const inputStatsElem = document.getElementById('input-stats');
        if (pipe.input.status === 'off') {
            playerElem.classList.add('hidden');
            inputStatsElem.classList.add('hidden');
            clearInputPreview(playerElem);
        } else {
            playerElem.classList.remove('hidden');
            inputStatsElem.classList.remove('hidden');
            renderInputPreview(playerElem, pipe);

            const video = pipe.input.video || {};
            const audio = pipe.input.audio || {};
            const stats = pipe.stats || {};
            const hasAudioTrack = !!audio.codec;

            document.getElementById('input-video-codec').textContent =
                formatCodecName(video.codec) || '--';
            document.getElementById('input-video-resolution').textContent =
                video.width && video.height ? video.width + 'x' + video.height : '--';
            document.getElementById('input-video-fps').textContent =
                video.fps !== null && video.fps !== undefined ? video.fps : '--';
            document.getElementById('input-video-level').textContent = video.level || '--';
            document.getElementById('input-video-profile').textContent = video.profile || '--';

            document.getElementById('input-audio-codec').textContent = hasAudioTrack
                ? formatCodecName(audio.codec) || audio.codec
                : 'No audio track';
            document.getElementById('input-audio-channels').textContent = hasAudioTrack
                ? audio.channels || '--'
                : '--';
            document.getElementById('input-audio-sample-rate').textContent = hasAudioTrack
                ? audio.sample_rate || '--'
                : '--';
            document.getElementById('input-audio-profile').textContent = hasAudioTrack
                ? audio.profile || '--'
                : '--';

            setBitrateWithSubtleUnit('input-total-bw', stats.inputBitrateKbps);
            setBitrateWithSubtleUnit('output-total-bw', stats.outputBitrateKbps);
            document.getElementById('input-reader-count').textContent =
                stats.readerCount !== null && stats.readerCount !== undefined
                    ? stats.readerCount
                    : '--';
            document.getElementById('input-output-count').textContent =
                stats.outputCount !== null && stats.outputCount !== undefined
                    ? stats.outputCount
                    : '--';
        }

        let publisherMeta = document.getElementById('publisher-meta');
        if (!publisherMeta) {
            publisherMeta = document.createElement('div');
            publisherMeta.id = 'publisher-meta';
            publisherMeta.className = 'mt-1 mb-4 flex flex-wrap items-center gap-2';
            inputStatsElem.parentNode.insertBefore(publisherMeta, inputStatsElem);
        }
        publisherMeta.replaceChildren();

        if (pipe.input.time !== null) {
            const uptimeBadge = document.createElement('span');
            uptimeBadge.className = 'badge text-sm px-3';
            uptimeBadge.textContent = msToHHMMSS(pipe.input.time);
            publisherMeta.appendChild(uptimeBadge);
        }

        const publisher = pipe.input.publisher;
        if (publisher) {
            const protoBadge = document.createElement('span');
            protoBadge.className = 'badge badge-info text-sm px-3';
            protoBadge.textContent = normalizePublisherProtocolLabel(publisher.protocol);
            publisherMeta.appendChild(protoBadge);

            if (publisher.remoteAddr) {
                const addrBadge = document.createElement('span');
                addrBadge.className = 'badge badge-outline font-mono text-sm px-3';
                addrBadge.textContent = publisher.remoteAddr;
                publisherMeta.appendChild(addrBadge);
            }

            const qualityAlerts = getPublisherQualityAlerts(publisher);
            const isHealthy = qualityAlerts.length === 0;
            const qualityBtn = document.createElement('button');
            qualityBtn.type = 'button';
            qualityBtn.className = `badge text-sm px-3 cursor-pointer ${isHealthy ? 'badge-success' : 'badge-warning'}`;
            qualityBtn.textContent = isHealthy ? 'Healthy' : 'Unhealthy';
            qualityBtn.addEventListener('click', () => {
                pipelineViewDependencies.openPublisherQualityModal?.(pipe.id);
            });
            publisherMeta.appendChild(qualityBtn);
        }

        const unexpectedCount = pipe.input.unexpectedReadersCount || 0;
        if (unexpectedCount > 0) {
            const urBadge = document.createElement('span');
            urBadge.className = 'badge badge-sm badge-error';
            urBadge.textContent = `${unexpectedCount} unexpected reader${unexpectedCount === 1 ? '' : 's'}`;
            publisherMeta.appendChild(urBadge);
        }
    }

    function renderOutsColumn(selectedPipe) {
        if (!selectedPipe) {
            document.getElementById('outs-col').classList.add('hidden');
            return;
        }

        document.getElementById('outs-col').classList.remove('hidden');

        const pipe = state.pipelines.find((p) => p.id === selectedPipe);
        if (!pipe) {
            console.error('Pipeline not found:', selectedPipe);
            return;
        }

        const outputsList = document.getElementById('outputs-list');
        outputsList.replaceChildren();

        pipe.outs.forEach((o, outputIndex) => {
            const statusColor =
                o.status === 'on'
                    ? 'status-primary'
                    : o.status === 'warning'
                      ? 'status-warning'
                      : o.status === 'error'
                        ? 'status-error'
                        : 'status-neutral';

            const isRunning = o.status === 'on' || o.status === 'warning';

            const row = document.createElement('div');
            row.className = 'bg-base-100 px-3 py-2 shadow rounded-box w-full';
            row.style.display = 'grid';
            row.style.gridTemplateColumns = 'minmax(0, 1fr) auto';
            row.style.gridTemplateRows = 'auto auto auto';
            row.style.alignItems = 'center';
            row.style.gap = '0.5rem';

            const content = document.createElement('div');
            content.className = 'min-w-0';

            const heading = document.createElement('div');
            heading.className = 'font-semibold flex items-center gap-2 min-w-0';

            const status = document.createElement('div');
            status.setAttribute('aria-label', 'status');
            status.className = `status status-lg ${statusColor} mx-1`;
            heading.appendChild(status);

            const toggleBtn = document.createElement('button');
            toggleBtn.className = `btn btn-xs ${isRunning ? 'btn-accent btn-outline' : 'btn-accent'}`;
            toggleBtn.dataset.outputIndex = String(outputIndex);
            toggleBtn.textContent = isRunning ? 'stop' : 'start';
            const toggleBusy = pipelineViewDependencies.isOutputToggleBusy?.(pipe.id, o.id);
            toggleBtn.disabled = !!toggleBusy;
            toggleBtn.classList.toggle('btn-disabled', !!toggleBusy);
            toggleBtn.addEventListener('click', async () => {
                if (toggleBtn.disabled) return;
                const out = pipe.outs[outputIndex];
                if (!out) return;
                toggleBtn.disabled = true;
                toggleBtn.classList.add('btn-disabled');
                try {
                    const running = out.status === 'on' || out.status === 'warning';
                    if (running) {
                        await pipelineViewDependencies.stopOutBtn?.(pipe.id, out.id, toggleBtn);
                    } else {
                        await pipelineViewDependencies.startOutBtn?.(pipe.id, out.id, toggleBtn);
                    }
                } finally {
                    const stillBusy = pipelineViewDependencies.isOutputToggleBusy?.(
                        pipe.id,
                        out.id,
                    );
                    if (!stillBusy) {
                        toggleBtn.disabled = false;
                        toggleBtn.classList.remove('btn-disabled');
                    }
                }
            });
            heading.appendChild(toggleBtn);

            const outputName = document.createElement('span');
            outputName.className = 'min-w-0 truncate';
            outputName.textContent = o.name;
            heading.appendChild(outputName);

            const desiredStateBadge = document.createElement('span');
            desiredStateBadge.className = `badge badge-sm whitespace-nowrap ${o.desiredState === 'running' ? 'badge-info' : 'badge-ghost'}`;
            desiredStateBadge.textContent = `intent: ${o.desiredState === 'running' ? 'run' : 'stop'}`;
            heading.appendChild(desiredStateBadge);

            const metadataRow = document.createElement('div');
            metadataRow.className =
                'mt-2 flex items-center gap-2 overflow-x-auto whitespace-nowrap';
            metadataRow.style.gridColumn = '1 / -1';

            if (o.time !== null) {
                const timeBadge = document.createElement('span');
                timeBadge.className = 'badge badge-sm whitespace-nowrap';
                timeBadge.textContent = msToHHMMSS(o.time);
                metadataRow.appendChild(timeBadge);
            }

            if (isRunning) {
                const throughputBadge = document.createElement('span');
                throughputBadge.className = 'badge badge-sm whitespace-nowrap';
                setBadgeBitrateWithSubtleUnit(throughputBadge, o.bitrateKbps);
                metadataRow.appendChild(throughputBadge);
            }

            if (o.totalSize) {
                const volumeBadge = document.createElement('span');
                volumeBadge.className = 'badge badge-sm whitespace-nowrap';
                volumeBadge.textContent = `${(Number(o.totalSize) / (1024 * 1024)).toFixed(1)} MB`;
                metadataRow.appendChild(volumeBadge);
            }

            const outputUrl = document.createElement('code');
            outputUrl.className = 'text-sm opacity-70 truncate block mt-1';
            outputUrl.textContent = sanitizeLogMessage(o.url, true);
            outputUrl.title = 'Hidden by default';
            outputUrl.style.gridColumn = '1 / -1';

            const actions = document.createElement('div');
            actions.className = 'flex items-center gap-2 self-start';

            const historyBtn = document.createElement('button');
            historyBtn.className = 'btn btn-xs btn-accent btn-outline';
            historyBtn.textContent = 'History';
            historyBtn.addEventListener('click', () => {
                pipelineViewDependencies.openOutputHistoryModal?.(pipe.id, o.id, o.name);
            });

            const editBtn = document.createElement('button');
            editBtn.className = 'btn btn-xs btn-accent btn-outline';
            editBtn.textContent = '✎';
            editBtn.addEventListener('click', () => {
                pipelineViewDependencies.editOutBtn?.(pipe.id, o.id);
            });

            const deleteBtn = document.createElement('button');
            deleteBtn.className = `btn btn-xs btn-accent btn-outline ${isRunning ? 'btn-disabled' : ''}`;
            deleteBtn.textContent = '✖';
            deleteBtn.addEventListener('click', () => {
                if (deleteBtn.classList.contains('btn-disabled')) return;
                pipelineViewDependencies.deleteOutBtn?.(pipe.id, o.id);
            });

            actions.appendChild(historyBtn);
            actions.appendChild(editBtn);
            actions.appendChild(deleteBtn);

            content.appendChild(heading);
            row.appendChild(content);
            row.appendChild(actions);
            if (metadataRow.childElementCount > 0) row.appendChild(metadataRow);
            row.appendChild(outputUrl);
            outputsList.appendChild(row);
        });
    }

export {
    renderPipelineInfoColumn,
    renderOutsColumn,
    setPipelineViewDependencies,
};
