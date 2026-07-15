const PREVIEW_LIVE_SYNC_SEGMENTS = 1;
const PREVIEW_MAX_LATENCY_SEGMENTS = 2;

export function buildPreviewHlsConfig(): Partial<HlsConfig> {
  return {
    startLevel: -1,
    lowLatencyMode: true,
    liveSyncDurationCount: PREVIEW_LIVE_SYNC_SEGMENTS,
    liveMaxLatencyDurationCount: PREVIEW_MAX_LATENCY_SEGMENTS,
    maxLiveSyncPlaybackRate: 1.5,
    backBufferLength: 6,
  };
}
