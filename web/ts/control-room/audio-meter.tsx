// An audio meter for a preview: the level of what the program carries,
// so "is the mic on?" has an answer without unmuting. The element is
// routed through Web Audio (analyser, then a gain at 0 unless Listen is
// on): metering needs the element's audio, which a muted element does
// not give, so silence comes from the gain instead.

import { useEffect, useRef, useState } from "react";

import { FLOOR_DB, levelDb, meterFraction } from "./audio-level.js";

/** How long a level must stay at the floor before the meter says so. */
const SILENT_AFTER_MS = 3_000;

export function AudioMeter(props: { element: HTMLVideoElement; context: AudioContext; label: string }) {
  const [db, setDb] = useState(FLOOR_DB);
  const [peak, setPeak] = useState(FLOOR_DB);
  const [silentSince, setSilentSince] = useState<number | null>(null);
  const [listening, setListening] = useState(false);
  const gain = useRef<GainNode | null>(null);

  useEffect(() => {
    const { element, context } = props;
    let source: MediaElementAudioSourceNode;
    try {
      source = context.createMediaElementSource(element);
    } catch {
      return; // already routed, or no Web Audio: no meter
    }
    const analyser = context.createAnalyser();
    analyser.fftSize = 2048;
    const out = context.createGain();
    out.gain.value = 0;
    gain.current = out;
    source.connect(analyser).connect(out).connect(context.destination);
    element.muted = false;
    void context.resume().catch(() => undefined);
    const block = new Float32Array(analyser.fftSize);
    let frame = 0;
    let held = FLOOR_DB;
    let heldAt = 0;
    let quietFrom: number | null = null;
    let last = 0;
    const tick = (now: number) => {
      frame = requestAnimationFrame(tick);
      if (now - last < 66) return; // ~15 updates a second is plenty to read
      last = now;
      analyser.getFloatTimeDomainData(block);
      const level = levelDb(block);
      if (level >= held || now - heldAt > 1_500) {
        held = level;
        heldAt = now;
      }
      quietFrom = level <= FLOOR_DB + 1 ? (quietFrom ?? Date.now()) : null;
      setDb(level);
      setPeak(held);
      setSilentSince(quietFrom);
    };
    frame = requestAnimationFrame(tick);
    return () => {
      cancelAnimationFrame(frame);
      source.disconnect();
      analyser.disconnect();
      out.disconnect();
      gain.current = null;
    };
  }, [props.element, props.context]);

  const silent = silentSince !== null && Date.now() - silentSince > SILENT_AFTER_MS;
  return (
    <div className="flex items-center gap-2 text-xs" role="group" aria-label={`Audio ${props.label}`}>
      <span className="text-base-content/70">Audio</span>
      <div
        role="meter"
        aria-label={`Audio level of ${props.label}`}
        aria-valuemin={FLOOR_DB}
        aria-valuemax={0}
        aria-valuenow={Math.round(db)}
        className="relative h-2 flex-1 overflow-hidden rounded-full bg-base-content/10"
      >
        <div
          className={`h-full rounded-full transition-[width] duration-75 ${db > -3 ? "bg-error" : db > -12 ? "bg-warning" : "bg-success"}`}
          style={{ width: `${meterFraction(db) * 100}%` }}
        />
        <div aria-hidden="true" className="absolute inset-y-0 w-0.5 bg-base-content/60" style={{ left: `${meterFraction(peak) * 100}%` }} />
      </div>
      <span className="tabular w-16 text-right text-base-content/70">{silent ? "silent" : db <= FLOOR_DB ? "--" : `${Math.round(db)} dB`}</span>
      <button
        type="button"
        className="btn btn-ghost btn-xs"
        aria-pressed={listening}
        onClick={() => {
          const next = !listening;
          if (gain.current) gain.current.gain.value = next ? 1 : 0;
          setListening(next);
        }}
      >
        {listening ? "Mute" : "Listen"}
      </button>
    </div>
  );
}
