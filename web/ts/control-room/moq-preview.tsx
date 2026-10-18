import { useEffect, useRef, useState } from "react";

import { mintMoqPreview } from "../core/api.js";

/** No decoded frame for this long after connecting means nothing plays. */
const FIRST_FRAME_MS = 15_000;

/** Whether this browser can play a MoQ preview (WebTransport + WebCodecs). */
export function moqPreviewSupported(): boolean {
  return typeof globalThis.WebTransport === "function" && typeof globalThis.VideoDecoder === "function";
}

type MoqState = "idle" | "starting" | "playing" | "buffering" | "failed";
const STATUS_TEXT: Record<Exclude<MoqState, "failed">, string> = {
  idle: "",
  starting: "connecting over WebTransport…",
  playing: "live (MoQ)",
  buffering: "buffering",
};

/** A muted live MoQ preview of the pipeline's program, straight from the
 * engine's MoQ origin over WebTransport. The player (`@moq/watch`) loads
 * only when asked. */
export function MoqPreview(props: { pipelineId: string; label: string }) {
  const canvas = useRef<HTMLCanvasElement | null>(null);
  /** 0 while off; each start (or retry) is a new attempt. */
  const [attempt, setAttempt] = useState(0);
  const [state, setState] = useState<MoqState>("idle");
  const [reason, setReason] = useState("");

  useEffect(() => {
    const element = canvas.current;
    if (attempt === 0 || !element) return;
    let ended = false;
    let release = () => {};
    const fail = (why: string) => {
      if (ended) return;
      end();
      setReason(why);
      setState("failed");
    };
    const end = () => {
      if (ended) return;
      ended = true;
      window.clearTimeout(firstFrame);
      release();
    };
    const firstFrame = window.setTimeout(() => fail("no frames: is the pipeline live?"), FIRST_FRAME_MS);

    void (async () => {
      const grant = await mintMoqPreview(props.pipelineId);
      if (ended) return;
      if (!grant) return fail("MoQ is off on this engine");
      const Watch = await import("@moq/watch");
      if (ended) return;
      const origin = new Watch.Net.Origin.Producer();
      const connect = (pin: string | null) =>
        Watch.Net.Connection.connect({
          url: new URL(grant.url),
          webtransport: pin ? { serverCertificateHashes: [{ algorithm: "sha-256", value: pin }] } : {},
          websocket: { enabled: false },
          consume: origin,
        });
      // Normal certificate checks first; pinning only if they fail (a
      // browser accepts a pinned certificate valid for at most 14 days).
      const session = await connect(null).catch((error: unknown) => {
        if (!grant.certificateSha256 || ended) throw error;
        return connect(grant.certificateSha256);
      });
      if (ended) return session.close();
      const player = new Watch.Player({
        origin,
        probe: session.probe,
        name: Watch.Net.Path.from(grant.broadcast),
        canvas: element,
        muted: true,
        visible: "always",
        // The token names the broadcast; subscribe without waiting for an announce.
        announced: false,
      });
      const unwatchFrames = player.video.out.timestamp.subscribe((timestamp) => {
        if (timestamp === undefined || ended) return;
        window.clearTimeout(firstFrame);
        setState((current) => (current === "starting" ? "playing" : current));
      });
      const unwatchStalls = player.video.out.stalled.subscribe((stalled) => {
        if (!ended) setState((current) => (current === "starting" ? current : stalled ? "buffering" : "playing"));
      });
      release = () => {
        unwatchFrames();
        unwatchStalls();
        player.close();
        session.close();
      };
      void session.closed.then((error) => fail(error ? error.message : "the engine ended the session"));
    })().catch((error: unknown) => fail(error instanceof Error ? error.message : String(error)));
    return end;
  }, [attempt, props.pipelineId]);

  const start = () => {
    setReason("");
    setState("starting");
    setAttempt((current) => current + 1);
  };
  const stop = () => {
    setAttempt(0);
    setState("idle");
  };
  const showing = state !== "idle" && state !== "failed";

  return (
    <div className="space-y-1" aria-label={`MoQ preview ${props.label}`} role="group">
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
          {state === "failed" ? `preview unavailable: ${reason}` : STATUS_TEXT[state]}
        </span>
      </div>
      {showing && (
        <canvas
          ref={canvas}
          className="aspect-video w-full rounded bg-black"
          aria-label={`Video ${props.label}`}
        />
      )}
    </div>
  );
}
