// An audio meter for a preview: the level of each channel of the track
// playing, so "is the mic on?" and "is one side dead?" have answers
// without unmuting. The element is routed through Web Audio (a splitter
// into one analyser per channel, and a gain at 0 unless Listen is on):
// metering needs the element's audio, which a muted element does not give,
// so silence comes from the gain instead.

import { useEffect, useRef, useState } from "react";

import { FLOOR_DB, channelLabels, levelDb, meterFraction } from "./audio-level.js";

/** How long a level must stay at the floor before the meter says so. */
const SILENT_AFTER_MS = 3_000;

interface ChannelLevel {
  db: number;
  peak: number;
}

export function AudioMeter(props: { element: HTMLVideoElement; context: AudioContext; label: string; channels: number }) {
  const labels = channelLabels(props.channels);
  const count = labels.length;
  const [levels, setLevels] = useState<ChannelLevel[]>(() => labels.map(() => ({ db: FLOOR_DB, peak: FLOOR_DB })));
  const [silentSince, setSilentSince] = useState<number | null>(null);
  const [listening, setListening] = useState(false);
  const gain = useRef<GainNode | null>(null);
  const source = useRef<MediaElementAudioSourceNode | null>(null);

  useEffect(() => {
    const { element, context } = props;
    // An element can be routed once: keep its source across channel changes.
    if (source.current === null || source.current.mediaElement !== element) {
      try {
        source.current = context.createMediaElementSource(element);
      } catch {
        return; // already routed elsewhere, or no Web Audio: no meter
      }
    }
    const input = source.current;
    const splitter = context.createChannelSplitter(count);
    const analysers = Array.from({ length: count }, () => {
      const analyser = context.createAnalyser();
      analyser.fftSize = 2048;
      return analyser;
    });
    analysers.forEach((analyser, channel) => splitter.connect(analyser, channel));
    const out = context.createGain();
    out.gain.value = gain.current?.gain.value ?? 0;
    gain.current = out;
    input.connect(splitter);
    input.connect(out).connect(context.destination);
    element.muted = false;
    void context.resume().catch(() => undefined);
    const block = new Float32Array(2048);
    const held = analysers.map(() => ({ db: FLOOR_DB, at: 0 }));
    let frame = 0;
    let quietFrom: number | null = null;
    let last = 0;
    const tick = (now: number) => {
      frame = requestAnimationFrame(tick);
      if (now - last < 66) return; // ~15 updates a second is plenty to read
      last = now;
      const next = analysers.map((analyser, channel) => {
        analyser.getFloatTimeDomainData(block);
        const db = levelDb(block);
        const hold = held[channel];
        if (db >= hold.db || now - hold.at > 1_500) {
          hold.db = db;
          hold.at = now;
        }
        return { db, peak: hold.db };
      });
      const loudest = Math.max(...next.map((level) => level.db));
      quietFrom = loudest <= FLOOR_DB + 1 ? (quietFrom ?? Date.now()) : null;
      setLevels(next);
      setSilentSince(quietFrom);
    };
    frame = requestAnimationFrame(tick);
    return () => {
      cancelAnimationFrame(frame);
      input.disconnect();
      splitter.disconnect();
      analysers.forEach((analyser) => analyser.disconnect());
      out.disconnect();
    };
  }, [props.element, props.context, count]);

  const loudest = Math.max(FLOOR_DB, ...levels.map((level) => level.db));
  const silent = silentSince !== null && Date.now() - silentSince > SILENT_AFTER_MS;
  return (
    <div className="flex items-center gap-2 text-xs" role="group" aria-label={`Audio ${props.label}`}>
      <span className="text-base-content/70">Audio</span>
      <div className="flex flex-1 flex-col gap-0.5">
        {labels.map((name, channel) => {
          const level = levels[channel] ?? { db: FLOOR_DB, peak: FLOOR_DB };
          return (
            <div key={name} className="flex items-center gap-1.5">
              {count > 1 && <span className="tabular w-6 text-[0.625rem] text-base-content/60">{name}</span>}
              <div
                role="meter"
                aria-label={count > 1 ? `Audio level of ${props.label}, channel ${name}` : `Audio level of ${props.label}`}
                aria-valuemin={FLOOR_DB}
                aria-valuemax={0}
                aria-valuenow={Math.round(level.db)}
                className={`relative flex-1 overflow-hidden rounded-full bg-base-content/10 ${count > 2 ? "h-1" : "h-1.5"}`}
              >
                <div
                  className={`h-full rounded-full transition-[width] duration-75 ${level.db > -3 ? "bg-error" : level.db > -12 ? "bg-warning" : "bg-success"}`}
                  style={{ width: `${meterFraction(level.db) * 100}%` }}
                />
                <div aria-hidden="true" className="absolute inset-y-0 w-0.5 bg-base-content/60" style={{ left: `${meterFraction(level.peak) * 100}%` }} />
              </div>
            </div>
          );
        })}
      </div>
      <span className="tabular w-16 text-right text-base-content/70">{silent ? "silent" : loudest <= FLOOR_DB ? "--" : `${Math.round(loudest)} dB`}</span>
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
