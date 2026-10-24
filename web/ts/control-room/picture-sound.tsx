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
import { audioTrackWords } from "./stream-health-model.js";

/** Loudness is drawn as LU above this floor, so the scale starts at 0. */
const FLOOR = -70;

export function PictureAndSound(props: { pipelineId: string; samples: readonly FlightSample[] }) {
  const canOperate = useSession().can("operator");
  const probe = usePolled(["probe", props.pipelineId], () => getPipelineProbe(props.pipelineId), 10_000);
  const qa = usePolled(["stream-qa", props.pipelineId], () => getStreamQa(props.pipelineId), 10_000);
  // What the last switch wrote, until the next poll reports the same.
  const [written, setWritten] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const polled = qa.value?.enabled ?? null;
  const enabled = written !== null && written !== polled ? written : (polled ?? false);
  const tracks: readonly ProbeAudioTrack[] = probe.value?.audioTracks ?? [];
  const measured = props.samples.some((sample) => sample.ingest?.qa?.loudnessLufs != null);
  return (
    <Panel
      id="health-picture-sound"
      title="Picture and sound"
      subtitle={tracks.length ? `${tracks.length} audio track${tracks.length === 1 ? "" : "s"}` : "no audio track reported"}
      actions={
        <label className="flex cursor-pointer items-center gap-2 text-sm" title={canOperate ? undefined : "Turning checks on needs the operator role"}>
          Picture and sound checks
          <input
            type="checkbox"
            role="switch"
            className="toggle toggle-primary toggle-sm"
            checked={enabled}
            disabled={!canOperate || busy || qa.value === null}
            onChange={async (event) => {
              setBusy(true);
              const saved = await setStreamQa(props.pipelineId, event.target.checked);
              if (saved) setWritten(saved.enabled);
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
            <p className="text-sm text-base-content/70">The publisher has not reported an audio track yet.</p>
          ) : (
            <ol className="flex flex-col gap-1.5" aria-label="Audio tracks">
              {tracks.map((track) => (
                <li key={track.trackIndex} className="flex items-baseline gap-2 rounded-field bg-base-100 px-3 py-2 text-sm">
                  <span className="font-medium">Track {track.trackIndex + 1}</span>
                  <span className="text-base-content/70">{audioTrackWords(track)}</span>
                  {track.trackIndex === Math.min(...tracks.map((each) => each.trackIndex)) && enabled && (
                    <span className="ml-auto text-xs text-base-content/70">checked</span>
                  )}
                </li>
              ))}
            </ol>
          )}
          {!enabled && (
            <p className="text-xs text-base-content/70">
              Checks are off. On, this server decodes one keyframe a second and the first audio track (about 1% of a CPU core) to catch a black or
              frozen picture, silence and loudness, and raises incidents for them.
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
              thresholds={[{ value: -14 - FLOOR, label: "-14 LUFS: platform level", tone: "ok" }]}
              series={[
                {
                  id: "lufs",
                  label: "LUFS (3 s)",
                  pick: (sample) => (sample.ingest?.qa?.loudnessLufs == null ? null : sample.ingest.qa.loudnessLufs - FLOOR),
                  color: "text-secondary",
                  area: true,
                  format: (value) => `${(value + FLOOR).toFixed(1)} LUFS`,
                },
              ]}
            />
          ) : (
            <p className="text-sm text-base-content/70">{enabled ? "Measuring: the first reading comes after 3 s of sound." : "Turn the checks on to measure loudness."}</p>
          )}
        </div>
      </div>
    </Panel>
  );
}
