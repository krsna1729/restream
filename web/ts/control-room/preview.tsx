import type Hls from "hls.js";
import type { HlsConfig, LoaderResponse, RetryConfig } from "hls.js";
import { useEffect, useMemo, useRef, useState } from "react";

import { programPreviewUrl } from "../core/api.js";
import { redirectToLogin } from "../core/auth-redirect.js";
import { declaredChannels } from "./audio-level.js";
import { AudioMeter } from "./audio-meter.js";
import { formatBps } from "./format.js";
import { appendSample, playbackReport, summarizePlayback } from "./playback-stats.js";
import type { PlaybackEvent, PlaybackSample } from "./playback-stats.js";
import { hlsDecodesHevc, programPreviewChoice } from "./preview-codec.js";

/** Seconds a preview may take to produce its first segment: the engine
 * starts a preview on its first request and answers 404 until then. */
const FIRST_SEGMENT_RETRIES = 30;
const RETRY_DELAY_MS = 500;
/** No new media for this long means the stream stopped (a dead source or a
 * stopped segmenter raise no hls.js error: the playlist just stops growing).
 * Longer than the first-segment wait, so a cold start is not a stall. */
const STALL_MS = 20_000;
const STALL_CHECK_MS = 1_000;

/** hls.js retries this many 404s (a preview warming up), each after
 * `RETRY_DELAY_MS`; other errors keep hls.js's own rule. */
function previewConfig(): Partial<HlsConfig> {
  const retry = {
    maxNumRetry: FIRST_SEGMENT_RETRIES,
    retryDelayMs: RETRY_DELAY_MS,
    maxRetryDelayMs: RETRY_DELAY_MS,
    shouldRetry: (
      config: RetryConfig | null | undefined,
      retryCount: number,
      _isTimeout: boolean,
      response: LoaderResponse | undefined,
      retry: boolean,
    ) => retry || (response?.code === 404 && retryCount < (config?.maxNumRetry ?? FIRST_SEGMENT_RETRIES)),
  };
  const policy = {
    default: {
      maxTimeToFirstByteMs: 10_000,
      maxLoadTimeMs: 20_000,
      timeoutRetry: { maxNumRetry: 2, retryDelayMs: 0, maxRetryDelayMs: 0 },
      errorRetry: retry,
    },
  };
  return {
    // Low latency for a monitor: one segment behind live, at most two.
    lowLatencyMode: true,
    liveSyncDurationCount: 1,
    liveMaxLatencyDurationCount: 2,
    maxLiveSyncPlaybackRate: 1.5,
    backBufferLength: 6,
    manifestLoadPolicy: policy,
    playlistLoadPolicy: policy,
  };
}

/** Safari plays HLS itself but does not retry a 404, so poll (as hls.js
 * retries) until the engine has the first segment, then call `done`. */
function awaitFirstSegment(
  url: string,
  alive: () => boolean,
  done: (outcome: "ready" | "login" | "missing") => void,
): void {
  let attempt = 0;
  const poll = () => {
    if (!alive()) return;
    void fetch(url, { credentials: "same-origin" })
      .catch(() => null)
      .then((response) => {
        if (!alive()) return;
        if (response?.ok) return done("ready");
        if (response?.status === 401) return done("login");
        attempt += 1;
        if (attempt > FIRST_SEGMENT_RETRIES) return done("missing");
        window.setTimeout(poll, RETRY_DELAY_MS);
      });
  };
  poll();
}

type PreviewState = "idle" | "starting" | "playing" | "buffering" | "paused" | "failed" | "stalled" | "unsupported";
const STATUS_TEXT: Record<PreviewState, string> = {
  idle: "",
  starting: "starting (the engine makes the first segment)…",
  playing: "live",
  buffering: "buffering",
  paused: "paused",
  failed: "preview unavailable",
  stalled: "stalled: no new frames",
  unsupported: "this browser cannot decode this stream (H.264 and AAC play in Chrome, Edge, Safari and Firefox)",
};

/** hls.js errors that mean the browser cannot decode the stream: retrying
 * will not help, so the preview says why. */
const CODEC_ERRORS = new Set(["manifestIncompatibleCodecsError", "bufferIncompatibleCodecsError", "bufferAddCodecError"]);

/** How a preview is drawn. `panel` (default) has its own start/stop
 * buttons, status line, track picker and meter. `tile` (the multiview wall)
 * plays from mount to unmount: the parent decides when it plays, and the
 * tile shows only the picture with its status over it. */
export type PreviewVariant = "panel" | "tile";

/** A muted live preview of an HLS URL, loaded only when asked: a preview
 * makes the engine segment the stream while it plays. With `fallbackUrl`,
 * a preview that fails before its first frame tries that URL once (an
 * H.265 source the browser claimed it decodes, then the H.264 copy). */
export function Preview(props: {
  url: string;
  label: string;
  fallbackUrl?: string | null;
  variant?: PreviewVariant;
}) {
  const tile = props.variant === "tile";
  const video = useRef<HTMLVideoElement | null>(null);
  const hls = useRef<Hls | null>(null);
  /** 0 while off; each start (or retry) is a new attempt. A tile starts
   * on mount. */
  const [attempt, setAttempt] = useState(tile ? 1 : 0);
  const [state, setState] = useState<PreviewState>(tile ? "starting" : "idle");
  const [tracks, setTracks] = useState<ReadonlyArray<{ id: number; name: string; channels: number }>>([]);
  const [track, setTrack] = useState<number | null>(null);
  /** Made in the click that starts a preview (browsers allow audio then);
   * the meter routes the element through it. */
  const [audio, setAudio] = useState<AudioContext | null>(null);
  const [element, setElement] = useState<HTMLVideoElement | null>(null);
  /** Set once the primary URL failed before playing and the fallback took over. */
  const [fellBack, setFellBack] = useState(false);
  const url = fellBack && props.fallbackUrl ? props.fallbackUrl : props.url;
  useEffect(() => () => void audio?.close().catch(() => undefined), [audio]);
  /** The player's readings once a second and its state changes, for the
   * stats overlay and its export (`playback-stats.ts`). Panel only. */
  const [samples, setSamples] = useState<PlaybackSample[]>([]);
  const events = useRef<PlaybackEvent[]>([]);
  const [statsOpen, setStatsOpen] = useState(false);
  const stateNow = useRef<PreviewState>(state);
  stateNow.current = state;
  useEffect(() => {
    if (tile || state === "idle") return;
    events.current = [...events.current.slice(-199), { atMs: Date.now(), kind: state, detail: fellBack ? "H.264 copy" : undefined }];
  }, [tile, state, fellBack]);
  useEffect(() => {
    if (tile || attempt === 0 || element === null) return;
    const timer = window.setInterval(() => {
      const quality = typeof element.getVideoPlaybackQuality === "function" ? element.getVideoPlaybackQuality() : null;
      const ranges = element.buffered;
      const edge = ranges.length > 0 ? ranges.end(ranges.length - 1) : element.currentTime;
      const player = hls.current;
      const loaded = element.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA;
      const reading: PlaybackSample = {
        atMs: Date.now(),
        state: stateNow.current,
        width: element.videoWidth,
        height: element.videoHeight,
        decodedFrames: quality && loaded ? quality.totalVideoFrames - quality.droppedVideoFrames : null,
        droppedFrames: quality && loaded ? quality.droppedVideoFrames : null,
        bufferAheadSec: Math.max(0, edge - element.currentTime),
        // Before media loads, hls.js reports its defaults (0 s behind, its
        // starting bandwidth guess), not measurements.
        latencySec: player && loaded && Number.isFinite(player.latency) ? player.latency : null,
        bandwidthBps: player && loaded && Number.isFinite(player.bandwidthEstimate) ? player.bandwidthEstimate : null,
      };
      setSamples((current) => appendSample(current, reading));
    }, 1_000);
    return () => window.clearInterval(timer);
  }, [tile, attempt, element]);
  const exportStats = () => {
    const report = playbackReport(
      { label: props.label, url, userAgent: navigator.userAgent, exportedAtMs: Date.now() },
      samples,
      events.current,
    );
    const link = document.createElement("a");
    link.href = URL.createObjectURL(new Blob([JSON.stringify(report, null, 2)], { type: "application/json" }));
    link.download = `playback-${props.label.replace(/[^a-z0-9]+/gi, "-")}-${new Date().toISOString().replace(/[:.]/g, "-")}.json`;
    link.click();
    window.setTimeout(() => URL.revokeObjectURL(link.href), 1_000);
  };

  useEffect(() => {
    const element = video.current;
    if (attempt === 0 || !element) return;
    let ended = false;
    let player: Hls | null = null;
    // New media is the buffered range moving. Seconds of playback are not:
    // hls.js seeks a waiting video forward, so currentTime can move with
    // nothing new to show. A video paused after it played (by the operator,
    // or a hidden tab) is not stalled: a player may stop loading once its
    // buffer is full (hls.js in Chromium keeps loading at the live edge).
    let bufferedEnd = 0;
    let grewAtMs = Date.now();
    let played = false;
    const watchdog = window.setInterval(() => {
      const ranges = element.buffered;
      const edge = ranges.length > 0 ? ranges.end(ranges.length - 1) : 0;
      // Any change counts: a reset timeline starts lower.
      if (edge !== bufferedEnd || (played && element.paused)) {
        bufferedEnd = edge;
        grewAtMs = Date.now();
      } else if (Date.now() - grewAtMs > STALL_MS) {
        fail("stalled");
      }
    }, STALL_CHECK_MS);
    const onPlaying = () => {
      played = true;
      setState("playing");
    };
    const onWaiting = () => setState("buffering");
    const onPause = () => setState("paused");
    element.addEventListener("playing", onPlaying);
    element.addEventListener("waiting", onWaiting);
    element.addEventListener("pause", onPause);
    const end = () => {
      if (ended) return;
      ended = true;
      window.clearInterval(watchdog);
      // Before load(): resetting the element may fire `pause`.
      element.removeEventListener("playing", onPlaying);
      element.removeEventListener("waiting", onWaiting);
      element.removeEventListener("pause", onPause);
      player?.destroy();
      hls.current = null;
      element.removeAttribute("src");
      // Without load() Safari keeps fetching the removed source.
      element.load();
      setTracks([]);
      setTrack(null);
    };
    const fail = (next: "failed" | "stalled" | "unsupported") => {
      end();
      if (!played && !fellBack && props.fallbackUrl) {
        setFellBack(true);
        setState("starting");
        return;
      }
      setState(next);
    };

    const playNatively = () =>
      awaitFirstSegment(url, () => !ended, (outcome) => {
        if (outcome === "login") return redirectToLogin();
        if (outcome === "missing") return fail("failed");
        element.src = url;
        void element.play().catch(() => fail("failed"));
      });
    // hls.js (about 600 KB) loads with the first preview, not the page.
    void import("hls.js").then(({ default: Hls }) => {
      if (ended) return;
      if (!Hls.isSupported()) return playNatively();
      player = new Hls(previewConfig());
      hls.current = player;
      player.on(Hls.Events.MANIFEST_PARSED, () => void element.play().catch(() => undefined));
      player.on(Hls.Events.AUDIO_TRACKS_UPDATED, () => {
        if (!player) return;
        setTracks(player.audioTracks.map(({ id, name, channels }) => ({ id, name, channels: declaredChannels(channels) })));
        setTrack(player.audioTrack);
      });
      // The selected track settles after the list arrives.
      player.on(Hls.Events.AUDIO_TRACK_SWITCHED, () => {
        if (player) setTrack(player.audioTrack);
      });
      player.on(Hls.Events.ERROR, (_event, data) => {
        if (typeof data !== "object" || data === null) return;
        if ("response" in data && typeof data.response === "object" && data.response !== null
          && "code" in data.response && data.response.code === 401) {
          redirectToLogin();
          return;
        }
        const codec = "details" in data && typeof data.details === "string" && CODEC_ERRORS.has(data.details);
        if ("fatal" in data && data.fatal) fail(codec ? "unsupported" : "failed");
      });
      player.loadSource(url);
      player.attachMedia(element);
    }, () => fail("failed"));
    return end;
  }, [attempt, url, fellBack, props.fallbackUrl]);

  const start = () => {
    if (!audio && typeof AudioContext !== "undefined") {
      try {
        setAudio(new AudioContext());
      } catch {
        /* no Web Audio: no meter */
      }
    }
    setTracks([]);
    setTrack(null);
    setSamples([]);
    events.current = [];
    setState("starting");
    setAttempt((current) => current + 1);
  };
  const stop = () => {
    setAttempt(0);
    setState("idle");
  };
  const ended = state === "failed" || state === "stalled" || state === "unsupported";
  const showing = state !== "idle" && !ended;

  if (tile) {
    return (
      <div className="relative aspect-video w-full overflow-hidden bg-black" role="group" aria-label={`Preview ${props.label}`}>
        <video
          ref={(node) => {
            video.current = node;
            setElement(node);
          }}
          className="size-full object-contain"
          muted
          playsInline
          aria-label={`Video ${props.label}`}
        />
        {state !== "playing" && (
          <div className="absolute inset-0 grid place-items-center p-2 text-center text-xs text-white/80">
            <span role="status" className="flex flex-col items-center gap-2">
              {STATUS_TEXT[state]}
              {ended && state !== "unsupported" && (
                <button type="button" className="btn btn-xs" onClick={start}>
                  Retry
                </button>
              )}
            </span>
          </div>
        )}
      </div>
    );
  }

  return (
    <div className="space-y-1" aria-label={`Preview ${props.label}`} role="group">
      <div className="flex flex-wrap items-center gap-2">
        {showing ? (
          <button type="button" className="btn btn-xs btn-ghost" onClick={stop}>
            Stop preview
          </button>
        ) : (
          <button type="button" className="btn btn-xs btn-outline" onClick={start}>
            {state === "idle" ? `Preview ${props.label}` : `Retry preview ${props.label}`}
          </button>
        )}
        <span role="status" className="text-xs text-base-content/70">
          {STATUS_TEXT[state]}
          {showing && props.fallbackUrl && (fellBack ? " · H.264 copy (H.265 did not play here)" : " · H.265 as sent")}
        </span>
        {showing && tracks.length > 1 && (
          <select
            className="select select-xs"
            aria-label={`Audio track of ${props.label}`}
            value={track ?? ""}
            onChange={(event) => {
              const next = Number(event.target.value);
              if (hls.current) hls.current.audioTrack = next;
              setTrack(next);
            }}
          >
            {tracks.map(({ id, name }) => (
              <option key={id} value={id}>
                {name}
              </option>
            ))}
          </select>
        )}
        {showing && (
          <button
            type="button"
            className="btn btn-ghost btn-xs"
            aria-pressed={statsOpen}
            onClick={() => setStatsOpen(!statsOpen)}
            title="How this browser plays the preview; the input's own health is on Stream health"
          >
            Player stats
          </button>
        )}
        {samples.length > 0 && (
          <button type="button" className="btn btn-ghost btn-xs" onClick={exportStats} title="Download this browser's playback readings as JSON">
            Export player stats
          </button>
        )}
      </div>
      {showing && (
        <div className="relative">
        <video
          ref={(node) => {
            video.current = node;
            setElement(node);
          }}
          className="aspect-video w-full rounded bg-black"
          muted
          playsInline
          controls
          aria-label={`Video ${props.label}`}
        />
        {statsOpen && <StatsOverlay samples={samples} />}
        </div>
      )}
      {showing && audio && element && state !== "starting" && (
        <AudioMeter
          element={element}
          context={audio}
          label={props.label}
          channels={(tracks.find((item) => item.id === track) ?? tracks[0])?.channels ?? 2}
        />
      )}
    </div>
  );
}

/** The player's readings over the picture, for "why does this preview
 * look bad?" without opening developer tools. */
function StatsOverlay(props: { samples: readonly PlaybackSample[] }) {
  const summary = summarizePlayback(props.samples);
  const rows: Array<[string, string]> = [
    ["picture", summary.resolution ?? "--"],
    ["frames", summary.fps === null ? "--" : `${summary.fps.toFixed(0)} fps`],
    ["dropped", summary.droppedShare === null ? "--" : `${(summary.droppedShare * 100).toFixed(1)}%`],
    ["buffer", summary.bufferAheadSec === null ? "--" : `${summary.bufferAheadSec.toFixed(1)} s ahead`],
    ["behind live", summary.latencySec === null ? "--" : `${summary.latencySec.toFixed(1)} s`],
    ["bandwidth", summary.bandwidthBps === null ? "--" : formatBps(summary.bandwidthBps)],
    ["stalls", String(summary.stalls)],
  ];
  return (
    <dl
      aria-label="Player stats (this browser)"
      className="pointer-events-none absolute left-2 top-2 grid grid-cols-[auto_auto] gap-x-3 rounded bg-black/70 px-2 py-1.5 font-mono text-[0.6875rem] leading-tight text-white"
    >
      <dt className="col-span-2 pb-0.5 text-white/70">player · this browser (input health: Stream health)</dt>
      {rows.map(([name, value]) => (
        <div key={name} className="contents">
          <dt className="text-white/70">{name}</dt>
          <dd className="tabular">{value}</dd>
        </div>
      ))}
    </dl>
  );
}

/** The program over HLS: an H.265 source this browser decodes plays as it
 * is (no FFmpeg on the server); otherwise, or if that fails, the H.264
 * copy. */
export function HlsProgramPreview(props: {
  pipelineId: string;
  sourceCodec: string | null;
  variant?: PreviewVariant;
}) {
  const choice = useMemo(
    () =>
      programPreviewChoice(
        {
          source: programPreviewUrl(props.pipelineId, { hevcPassthrough: true }),
          h264: programPreviewUrl(props.pipelineId),
        },
        props.sourceCodec,
        hlsDecodesHevc(),
      ),
    [props.pipelineId, props.sourceCodec],
  );
  return (
    <Preview key={choice.url} url={choice.url} fallbackUrl={choice.fallbackUrl} label="program" variant={props.variant} />
  );
}
