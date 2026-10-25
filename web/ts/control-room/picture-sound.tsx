// Stream health's "Picture and sound": the audio tracks the publisher
// sends, and the engine's picture and sound checks (black, still,
// silence, loudness) with their switch. The checks decode on this server,
// so they are opt in per pipeline.

import { useState } from "react";

import { getPipelineProbe, getStreamQa, setStreamQa } from "../core/api.js";
import type { FlightSample, ProbeAudioTrack } from "../core/api-types.js";
import { Panel } from "../ui/panel.js";
import { TimeSeries } from "../ui/time-series.js";
import { usePolled } from "./polled.js";
import { useSession } from "./session.js";
import {
  LOUDNESS_FLOOR_LUFS,
  LOUDNESS_TARGET_LUFS,
  audioTrackWords,
  loudnessFraction,
  qaCostWords,
  trackMeter,
} from "./stream-health-model.js";
import type { Severity } from "./stream-health-model.js";

/** One colour per track on the loudness chart, in track order. */
const TRACK_COLORS = [
  "text-secondary",
  "text-accent",
  "text-info",
  "text-warning",
  "text-primary",
  "text-success",
];
const METER_FILL: Record<Severity, string> = {
  ok: "bg-success",
  warn: "bg-warning",
  bad: "bg-error",
};

/** Loudness is drawn as LU above this floor, so the scale starts at 0. */
const FLOOR = -70;

export function PictureAndSound(props: {
  pipelineId: string;
  samples: readonly FlightSample[];
}) {
  const canOperate = useSession().can("operator");
  const probe = usePolled(
    ["probe", props.pipelineId],
    () => getPipelineProbe(props.pipelineId),
    10_000,
  );
  const qa = usePolled(
    ["stream-qa", props.pipelineId],
    () => getStreamQa(props.pipelineId),
    10_000,
  );
  // What the last switch wrote, until the next poll reports the same.
  const [written, setWritten] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const polled = qa.value?.enabled ?? null;
  const enabled =
    written !== null && written !== polled ? written : (polled ?? false);
  const tracks: readonly ProbeAudioTrack[] = probe.value?.audioTracks ?? [];
  const measured = props.samples.some(
    (sample) => sample.ingest?.qa?.loudnessLufs != null,
  );
  // One line per track the checks measured (at most the colours we have).
  const chartTracks = Math.min(
    TRACK_COLORS.length,
    Math.max(
      0,
      ...props.samples.map((sample) => sample.ingest?.qa?.tracks?.length ?? 0),
    ),
  );
  return (
    <Panel
      id="health-picture-sound"
      title="Picture and sound"
      subtitle={
        tracks.length
          ? `${tracks.length} audio track${tracks.length === 1 ? "" : "s"}`
          : "no audio track reported"
      }
      actions={
        <label
          className="flex cursor-pointer items-center gap-2 text-sm"
          title={
            canOperate ? undefined : "Turning checks on needs the operator role"
          }
        >
          Picture and sound checks
          <input
            type="checkbox"
            role="switch"
            className="toggle toggle-primary toggle-sm"
            checked={enabled}
            disabled={!canOperate || busy || qa.value === null}
            onChange={async (event) => {
              const wanted = event.target.checked;
              // Show the change at once; the engine's answer settles it, and
              // a failed save puts the switch back.
              const before = enabled;
              setWritten(wanted);
              setBusy(true);
              const saved = await setStreamQa(props.pipelineId, wanted);
              setWritten(saved ? saved.enabled : before);
              qa.refresh();
              setBusy(false);
            }}
          />
        </label>
      }
    >
      <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_minmax(0,1.6fr)]">
        <div className="flex flex-col gap-2">
          <h3 className="kicker">Audio tracks</h3>
          {tracks.length === 0 ? (
            <p className="text-sm text-base-content/70">
              The publisher has not reported an audio track yet.
            </p>
          ) : (
            <ol className="flex flex-col gap-1.5" aria-label="Audio tracks">
              {tracks.map((track, index) => (
                <li
                  key={track.trackIndex}
                  className="flex flex-col gap-1.5 rounded-field bg-base-100 px-3 py-2 text-sm"
                >
                  <span className="flex items-baseline gap-2">
                    <span className="font-medium">
                      Track {track.trackIndex + 1}
                    </span>
                    <span className="text-base-content/70">
                      {audioTrackWords(track)}
                    </span>
                  </span>
                  {enabled && (
                    <TrackLoudness
                      label={`Track ${track.trackIndex + 1}`}
                      meter={trackMeter(
                        qa.value?.findings?.tracks?.[index],
                        qa.value?.findings?.soundCheckedMs ?? null,
                        Date.now(),
                      )}
                    />
                  )}
                </li>
              ))}
            </ol>
          )}
          {!enabled && (
            <p className="text-xs text-base-content/70">
              Checks are off. On, this server runs a small FFmpeg that decodes
              the keyframes and every audio track to catch a black or frozen
              picture, a slate, silence and loudness, and raises incidents for
              them. For this pipeline that costs {qaCostWords(tracks.length)}.
            </p>
          )}
        </div>
        <div className="min-w-0">
          <h3 className="kicker mb-2">Loudness</h3>
          {enabled && measured ? (
            <TimeSeries
              samples={props.samples}
              label="Loudness over time"
              height={120}
              top={-FLOOR}
              axisFormat={(value) => `${Math.round(value + FLOOR)}`}
              thresholds={[
                {
                  value: -14 - FLOOR,
                  label: "-14 LUFS: platform level",
                  tone: "ok",
                },
              ]}
              series={Array.from(
                { length: Math.max(1, chartTracks) },
                (_, index) => ({
                  id: `lufs-${index}`,
                  label: chartTracks > 1 ? `Track ${index + 1}` : "LUFS (3 s)",
                  pick: (sample: FlightSample) => {
                    const qaSample = sample.ingest?.qa;
                    // Older samples carry only the first track's figure.
                    const lufs =
                      qaSample?.tracks?.[index]?.loudnessLufs ??
                      (index === 0 ? qaSample?.loudnessLufs : null);
                    return lufs == null ? null : lufs - FLOOR;
                  },
                  color: TRACK_COLORS[index % TRACK_COLORS.length],
                  area: chartTracks <= 1,
                  format: (value: number) =>
                    `${(value + FLOOR).toFixed(1)} LUFS`,
                }),
              )}
            />
          ) : (
            <p className="text-sm text-base-content/70">
              {enabled
                ? "Measuring: the first reading comes after 3 s of sound."
                : "Turn the checks on to measure loudness."}
            </p>
          )}
        </div>
      </div>
    </Panel>
  );
}

/** A track's loudness on a bar, with the platform target marked. */
function TrackLoudness(props: {
  label: string;
  meter: ReturnType<typeof trackMeter>;
}) {
  const { meter } = props;
  return (
    <span className="flex items-center gap-2 text-xs">
      <span
        role="meter"
        aria-label={`Loudness of ${props.label}`}
        aria-valuemin={LOUDNESS_FLOOR_LUFS}
        aria-valuemax={0}
        aria-valuenow={
          meter
            ? Math.round(
                LOUDNESS_FLOOR_LUFS + meter.fraction * -LOUDNESS_FLOOR_LUFS,
              )
            : undefined
        }
        aria-valuetext={meter?.words ?? "not measured yet"}
        className="relative h-2 flex-1 overflow-hidden rounded-full bg-base-content/10"
      >
        {meter && (
          <span
            className={`block h-full rounded-full ${METER_FILL[meter.severity]}`}
            style={{ width: `${meter.fraction * 100}%` }}
          />
        )}
        <span
          aria-hidden="true"
          title={`${LOUDNESS_TARGET_LUFS} LUFS: platform level`}
          className="absolute inset-y-0 w-0.5 bg-base-content/60"
          style={{ left: `${loudnessFraction(LOUDNESS_TARGET_LUFS) * 100}%` }}
        />
      </span>
      <span className="tabular w-16 text-right text-base-content/70">
        {meter?.words ?? "checking"}
      </span>
    </span>
  );
}
