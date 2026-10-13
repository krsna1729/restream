import { useEffect, useRef, useState } from "react";

import { redirectToLogin } from "../core/auth-redirect.js";

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
      config: { maxNumRetry: number },
      retryCount: number,
      _isTimeout: boolean,
      response: { code?: number } | undefined,
      retry: boolean,
    ) => retry || (response?.code === 404 && retryCount < config.maxNumRetry),
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

type PreviewState = "idle" | "starting" | "playing" | "buffering" | "paused" | "failed" | "stalled";
const STATUS_TEXT: Record<PreviewState, string> = {
  idle: "",
  starting: "starting (the engine makes the first segment)…",
  playing: "live",
  buffering: "buffering",
  paused: "paused",
  failed: "preview unavailable",
  stalled: "stalled: no new frames",
};

/** A muted live preview of an HLS URL, loaded only when asked: a preview
 * makes the engine segment the stream while it plays. */
export function Preview(props: { url: string; label: string }) {
  const video = useRef<HTMLVideoElement | null>(null);
  const hls = useRef<Hls | null>(null);
  /** 0 while off; each start (or retry) is a new attempt. */
  const [attempt, setAttempt] = useState(0);
  const [state, setState] = useState<PreviewState>("idle");
  const [tracks, setTracks] = useState<ReadonlyArray<{ id: number; name: string }>>([]);
  const [track, setTrack] = useState<number | null>(null);

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
    const fail = (next: "failed" | "stalled") => {
      end();
      setState(next);
    };

    const Hls = window.Hls;
    if (Hls?.isSupported()) {
      player = new Hls(previewConfig());
      hls.current = player;
      player.on(Hls.Events.MANIFEST_PARSED, () => void element.play().catch(() => undefined));
      player.on(Hls.Events.AUDIO_TRACKS_UPDATED, () => {
        if (!player) return;
        setTracks(player.audioTracks.map(({ id, name }) => ({ id, name })));
        setTrack(player.audioTrack);
      });
      player.on(Hls.Events.ERROR, (_event, data) => {
        if (typeof data !== "object" || data === null) return;
        if ("response" in data && typeof data.response === "object" && data.response !== null
          && "code" in data.response && data.response.code === 401) {
          redirectToLogin();
          return;
        }
        if ("fatal" in data && data.fatal) fail("failed");
      });
      player.loadSource(props.url);
      player.attachMedia(element);
    } else {
      awaitFirstSegment(props.url, () => !ended, (outcome) => {
        if (outcome === "login") return redirectToLogin();
        if (outcome === "missing") return fail("failed");
        element.src = props.url;
        void element.play().catch(() => fail("failed"));
      });
    }
    return end;
  }, [attempt, props.url]);

  const start = () => {
    setTracks([]);
    setTrack(null);
    setState("starting");
    setAttempt((current) => current + 1);
  };
  const stop = () => {
    setAttempt(0);
    setState("idle");
  };
  const showing = state !== "idle" && state !== "failed" && state !== "stalled";

  return (
    <div className="space-y-1" aria-label={`Preview ${props.label}`} role="group">
      <div className="flex flex-wrap items-center gap-2">
        {showing ? (
          <button type="button" className="btn btn-xs btn-ghost" onClick={stop}>
            Stop preview
          </button>
        ) : (
          <button type="button" className="btn btn-xs" onClick={start}>
            {state === "idle" ? `Preview ${props.label}` : `Retry preview ${props.label}`}
          </button>
        )}
        <span role="status" className="text-xs opacity-70">
          {STATUS_TEXT[state]}
        </span>
        {showing && tracks.length > 1 && (
          <select
            className="select select-xs select-bordered"
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
      </div>
      {showing && (
        <video
          ref={video}
          className="aspect-video w-full rounded bg-black"
          muted
          playsInline
          controls
          aria-label={`Video ${props.label}`}
        />
      )}
    </div>
  );
}
