import { useEffect, useRef, useState } from "react";

/** Seconds a preview may take to produce its first segment: the engine
 * starts a preview on its first request and answers 404 until then. */
const FIRST_SEGMENT_RETRIES = 30;
const RETRY_DELAY_MS = 500;

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

type PreviewState = "idle" | "starting" | "playing" | "failed";

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
    const Hls = window.Hls;
    if (!Hls?.isSupported()) {
      // Safari plays HLS natively.
      element.src = props.url;
      void element.play().catch(() => setState("failed"));
      return () => element.removeAttribute("src");
    }
    const player = new Hls(previewConfig());
    hls.current = player;
    let destroyed = false;
    const destroy = () => {
      if (destroyed) return;
      destroyed = true;
      player.destroy();
      hls.current = null;
    };
    player.on(Hls.Events.MANIFEST_PARSED, () => void element.play().catch(() => undefined));
    player.on(Hls.Events.AUDIO_TRACKS_UPDATED, () => {
      setTracks(player.audioTracks.map(({ id, name }) => ({ id, name })));
      setTrack(player.audioTrack);
    });
    player.on(Hls.Events.ERROR, (_event, data) => {
      if (typeof data === "object" && data !== null && "fatal" in data && data.fatal) {
        destroy();
        setState("failed");
      }
    });
    player.loadSource(props.url);
    player.attachMedia(element);
    return destroy;
  }, [attempt, props.url]);

  const start = () => {
    setState("starting");
    setAttempt((current) => current + 1);
  };
  const stop = () => {
    setAttempt(0);
    setTracks([]);
    setState("idle");
  };

  return (
    <div className="space-y-1" aria-label={`Preview ${props.label}`} role="group">
      <div className="flex flex-wrap items-center gap-2">
        {state === "idle" || state === "failed" ? (
          <button type="button" className="btn btn-xs" onClick={start}>
            {state === "failed" ? `Retry preview ${props.label}` : `Preview ${props.label}`}
          </button>
        ) : (
          <button type="button" className="btn btn-xs btn-ghost" onClick={stop}>
            Stop preview
          </button>
        )}
        <span role="status" className="text-xs opacity-70">
          {state === "starting"
            ? "starting (the engine makes the first segment)…"
            : state === "playing"
              ? "live"
              : state === "failed"
                ? "preview unavailable"
                : ""}
        </span>
        {tracks.length > 1 && (
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
      {state !== "idle" && state !== "failed" && (
        <video
          ref={video}
          className="aspect-video w-full rounded bg-black"
          muted
          playsInline
          controls
          aria-label={`Video ${props.label}`}
          onPlaying={() => setState("playing")}
        />
      )}
    </div>
  );
}
