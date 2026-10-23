import { useEffect, useState } from "react";

import {
  detectAudioPlatform,
  detectAudioProtocol,
  getAudioCaps,
  getAudioPlatformLabel,
  loadAudioCaps,
} from "../core/audio-caps.js";
import { createOutput, getPipelineProbe, updateOutput } from "../core/api.js";
import type { ProbeAudioTrack } from "../core/api-types.js";
import { OUTPUT_SERVER_PRESETS } from "../core/output-url.js";
import type { ConfigOutput } from "../types.js";
import {
  audioChoiceCounts,
  audioNotes,
  outputDraft,
  outputProblem,
  outputUrl,
  outputWrite,
} from "./manage-model.js";
import type { AudioMode, OutputDraft, OutputProtocol } from "./manage-model.js";
import { SettingsForm } from "./settings-form.js";

const AUDIO_MODES: ReadonlyArray<[AudioMode, string]> = [
  ["all", "Pass all tracks through"],
  ["selectTracks", "Send chosen tracks"],
  ["downmix", "Downmix one track to stereo"],
  ["remap", "Two channels of one track"],
];

/** Add a destination (`stored` null) or change one. The source's audio
 * tracks come from the live publisher; the limits from the engine's caps
 * table for the destination's platform and protocol. */
export function OutputForm(props: {
  pipelineId: string;
  stored: ConfigOutput | null;
  profiles: readonly string[];
  /** The host a new SRT destination starts with. */
  defaultHost: string;
  onSaved: (output: ConfigOutput) => void;
  onClose: () => void;
}) {
  const { stored, pipelineId } = props;
  const [draft, setDraft] = useState<OutputDraft>(() => outputDraft(stored, props.defaultHost));
  const [tracks, setTracks] = useState<readonly ProbeAudioTrack[]>([]);
  const [, setCapsLoaded] = useState(false);
  useEffect(() => {
    void loadAudioCaps().then(() => setCapsLoaded(true));
    void getPipelineProbe(pipelineId).then((probe) => setTracks(probe?.audioTracks ?? []));
  }, [pipelineId]);
  const set = (next: Partial<OutputDraft>) => setDraft({ ...draft, ...next });
  const setSrt = (next: Partial<OutputDraft["srt"]>) => set({ srt: { ...draft.srt, ...next } });

  const url = outputUrl(draft);
  const platform = detectAudioPlatform(url);
  const caps = getAudioCaps(platform, detectAudioProtocol(url, draft.protocol));
  const presets = OUTPUT_SERVER_PRESETS[draft.protocol] ?? [];
  const firstTrack = draft.tracks[0] ?? 0;
  const choices = audioChoiceCounts(draft, tracks);
  const id = stored ? `output-edit-${stored.id}` : "output-new";

  return (
    <SettingsForm
      id={id}
      title={stored ? `Destination ${stored.name}` : "New destination"}
      problem={outputProblem(draft)}
      onSave={async () => {
        const body = outputWrite(draft);
        const saved = stored
          ? await updateOutput(pipelineId, stored.id, body)
          : await createOutput(pipelineId, body);
        if (saved === null) return false;
        props.onSaved(saved.output);
        return true;
      }}
    >
      <div className="grid gap-2 md:grid-cols-2">
        <label className="form-control">
          <span className="label-text">Name</span>
          <input
            className="input input-sm input-bordered"
            value={draft.name}
            onChange={(event) => set({ name: event.target.value })}
          />
        </label>
        <label className="form-control">
          <span className="label-text">Protocol</span>
          <select
            className="select select-sm select-bordered"
            value={draft.protocol}
            onChange={(event) =>
              // A URL of one protocol never fits another: start the new one empty.
              set({ protocol: event.target.value as OutputProtocol, server: "", input: "" })
            }
          >
            <option value="rtmp">RTMP / RTMPS</option>
            <option value="srt">SRT</option>
            <option value="hls">HLS (HTTP upload)</option>
          </select>
        </label>
      </div>
      {draft.protocol === "srt" ? (
        <div className="grid gap-2 md:grid-cols-3">
          <label className="form-control">
            <span className="label-text">Host</span>
            <input
              className="input input-sm input-bordered"
              value={draft.srt.host}
              onChange={(event) => setSrt({ host: event.target.value })}
            />
          </label>
          <label className="form-control">
            <span className="label-text">Port</span>
            <input
              className="input input-sm input-bordered"
              value={draft.srt.port}
              onChange={(event) => setSrt({ port: event.target.value })}
            />
          </label>
          <label className="form-control">
            <span className="label-text">Stream ID</span>
            <input
              className="input input-sm input-bordered font-mono"
              value={draft.srt.streamId}
              onChange={(event) => setSrt({ streamId: event.target.value })}
            />
          </label>
          <label className="form-control">
            <span className="label-text">Passphrase (empty: none)</span>
            <input
              type="password"
              autoComplete="off"
              className="input input-sm input-bordered"
              value={draft.srt.passphrase}
              onChange={(event) => setSrt({ passphrase: event.target.value })}
            />
          </label>
          <label className="form-control">
            <span className="label-text">Key length</span>
            <select
              className="select select-sm select-bordered"
              value={draft.srt.pbkeylen}
              onChange={(event) => setSrt({ pbkeylen: event.target.value })}
            >
              <option value="16">AES-128</option>
              <option value="24">AES-192</option>
              <option value="32">AES-256</option>
            </select>
          </label>
          <label className="form-control">
            <span className="label-text">Other options (a=b&amp;c=d)</span>
            <input
              className="input input-sm input-bordered font-mono"
              value={draft.srt.extraQuery}
              onChange={(event) => setSrt({ extraQuery: event.target.value })}
            />
          </label>
        </div>
      ) : (
        <div className="grid gap-2 md:grid-cols-3">
          <label className="form-control">
            <span className="label-text">Server</span>
            <select
              className="select select-sm select-bordered"
              value={draft.server}
              onChange={(event) => set({ server: event.target.value })}
            >
              {presets.map((preset) => (
                <option key={preset.label} value={preset.value}>
                  {preset.value ? preset.label : "Custom URL"}
                </option>
              ))}
            </select>
          </label>
          <label className="form-control md:col-span-2">
            <span className="label-text">{draft.server ? "Stream key" : "URL"}</span>
            <input
              className="input input-sm input-bordered font-mono"
              autoComplete="off"
              value={draft.input}
              onChange={(event) => set({ input: event.target.value })}
            />
          </label>
          {draft.protocol === "rtmp" && !draft.server && (
            <label className="form-control">
              <span className="label-text">RTMP flavour</span>
              <select
                className="select select-sm select-bordered"
                value={draft.rtmpMode}
                onChange={(event) =>
                  set({ rtmpMode: event.target.value === "enhanced" ? "enhanced" : "legacy" })
                }
              >
                <option value="legacy">legacy (H.264)</option>
                <option value="enhanced">enhanced (H.265 capable)</option>
              </select>
            </label>
          )}
        </div>
      )}
      <p className="break-all font-mono text-xs text-base-content/70" aria-label="Destination URL">
        {url || "—"}
      </p>
      <label className="form-control">
        <span className="label-text">Monitoring URL (optional, e.g. the YouTube watch page)</span>
        <input
          className="input input-sm input-bordered"
          value={draft.monitoringUrl}
          onChange={(event) => set({ monitoringUrl: event.target.value })}
        />
      </label>
      <div className="grid gap-2 md:grid-cols-2">
        <label className="form-control">
          <span className="label-text">Video</span>
          <select
            className="select select-sm select-bordered"
            value={draft.profile === null ? "c:" : draft.profile ? `p:${draft.profile}` : "s:"}
            onChange={(event) => {
              const value = event.target.value;
              set({ profile: value === "c:" ? null : value.startsWith("p:") ? value.slice(2) : "" });
            }}
          >
            {draft.customVideo && <option value="c:">Custom settings (kept as stored)</option>}
            <option value="s:">Source (no transcode)</option>
            {props.profiles.map((name) => (
              <option key={name} value={`p:${name}`}>
                Profile {name}
              </option>
            ))}
          </select>
        </label>
        <label className="form-control">
          <span className="label-text">Video codec</span>
          <select
            className="select select-sm select-bordered"
            value={draft.codec}
            disabled={draft.profile === null}
            onChange={(event) => set({ codec: event.target.value as OutputDraft["codec"] })}
          >
            <option value="auto">auto</option>
            <option value="h264">H.264</option>
            <option value="h265">H.265</option>
          </select>
        </label>
      </div>
      <fieldset className="space-y-2">
        <legend className="label-text">Audio</legend>
        <p className="flex flex-wrap gap-1 text-xs" aria-label="Audio limits">
          <span className="badge badge-sm badge-ghost">
            {getAudioPlatformLabel(platform)} · {detectAudioProtocol(url, draft.protocol).toUpperCase()}
          </span>
          <span className="badge badge-sm badge-ghost">
            {caps.maxTracks === Infinity ? "any" : caps.maxTracks} tracks
          </span>
          <span className="badge badge-sm badge-ghost">
            {caps.maxChannels === Infinity ? "any" : caps.maxChannels} channels
          </span>
          <span className="badge badge-sm badge-ghost">
            codecs {caps.codecs === "any" ? "any" : caps.codecs.join(", ")}
          </span>
        </p>
        <select
          className="select select-sm select-bordered"
          aria-label="Audio routing"
          value={draft.audio}
          onChange={(event) => set({ audio: event.target.value as AudioMode })}
        >
          {AUDIO_MODES.map(([mode, label]) => (
            <option key={mode} value={mode}>
              {label}
            </option>
          ))}
        </select>
        {draft.audio === "selectTracks" && (
          <div className="flex flex-wrap gap-3" role="group" aria-label="Tracks to send">
            {Array.from({ length: choices.tracks }, (_, position) => (
              <label key={position} className="label cursor-pointer gap-2">
                <input
                  type="checkbox"
                  className="checkbox checkbox-sm"
                  checked={draft.tracks.includes(position)}
                  onChange={(event) =>
                    set({
                      tracks: event.target.checked
                        ? [...draft.tracks, position].sort((a, b) => a - b)
                        : draft.tracks.filter((track) => track !== position),
                    })
                  }
                />
                <span className="label-text">{trackLabel(tracks, position)}</span>
              </label>
            ))}
          </div>
        )}
        {(draft.audio === "downmix" || draft.audio === "remap") && (
          <div className="flex flex-wrap gap-2">
            <select
              className="select select-sm select-bordered"
              aria-label="Source track"
              value={firstTrack}
              onChange={(event) => set({ tracks: [Number(event.target.value)] })}
            >
              {Array.from({ length: choices.tracks }, (_, position) => (
                <option key={position} value={position}>
                  {trackLabel(tracks, position)}
                </option>
              ))}
            </select>
            {draft.audio === "remap" &&
              (["leftChannel", "rightChannel"] as const).map((side) => (
                <select
                  key={side}
                  className="select select-sm select-bordered"
                  aria-label={side === "leftChannel" ? "Left from channel" : "Right from channel"}
                  value={draft[side]}
                  onChange={(event) => {
                    const channel = Number(event.target.value);
                    set(side === "leftChannel" ? { leftChannel: channel } : { rightChannel: channel });
                  }}
                >
                  {Array.from({ length: choices.channels }, (_, channel) => (
                    <option key={channel} value={channel}>
                      {side === "leftChannel" ? "Left" : "Right"} ← channel {channel + 1}
                    </option>
                  ))}
                </select>
              ))}
          </div>
        )}
        {audioNotes(draft, caps, tracks).map((note, index) => (
          <p key={index} className="text-xs text-base-content/70">
            {note}
          </p>
        ))}
      </fieldset>
      <button type="button" className="btn btn-sm btn-ghost self-start" onClick={props.onClose}>
        Close
      </button>
    </SettingsForm>
  );
}

function trackLabel(tracks: readonly ProbeAudioTrack[], position: number): string {
  const track = tracks[position];
  if (!track) return `Track ${position + 1}`;
  const parts = [track.codec, track.channelLayout ?? (track.channels ? `${track.channels} ch` : null)];
  return `Track ${position + 1} (${parts.filter(Boolean).join(", ")})`;
}
