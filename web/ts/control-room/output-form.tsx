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
import type { ConfigOutput } from "../types.js";
import { Field } from "../ui/controls.js";
import { platformOf } from "./channels-model.js";
import {
  applyChoice,
  audioChoiceCounts,
  choiceKey,
  destinationChoices,
  audioNotes,
  outputDraft,
  outputProblem,
  outputUrl,
  outputWrite,
} from "./manage-model.js";
import type { AudioMode, OutputDraft, OutputProtocol } from "./manage-model.js";
import { PlatformMark } from "./platform-mark.js";
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
  const firstTrack = draft.tracks[0] ?? 0;
  const choicesCount = audioChoiceCounts(draft, tracks);
  const id = stored ? `output-edit-${stored.id}` : "output-new";

  const choices = destinationChoices();
  const chosen = choices.find((choice) => choice.key === choiceKey(draft)) ?? null;
  const videoSummary = draft.profile === null ? "custom video" : draft.profile ? `profile ${draft.profile}` : "source video";
  const audioSummary = AUDIO_MODES.find(([mode]) => mode === draft.audio)?.[1].toLowerCase() ?? draft.audio;

  return (
    <SettingsForm
      id={id}
      title={stored ? `Destination ${stored.name}` : "New destination"}
      problem={outputProblem(draft)}
      actions={
        <button type="button" className="btn btn-sm btn-ghost" onClick={props.onClose}>
          {stored ? "Close" : "Cancel"}
        </button>
      }
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
      <fieldset className="flex min-w-0 flex-col gap-2">
        <legend className="kicker mb-2">Where to send</legend>
        <div role="radiogroup" aria-label="Destination type" className="grid grid-cols-2 gap-2 sm:grid-cols-3 xl:grid-cols-5">
          {choices.map((choice) => (
            <label
              key={choice.key}
              className="relative flex min-w-0 cursor-pointer items-center gap-2.5 rounded-box border border-base-content/15 bg-base-100 p-2 text-sm transition-colors hover:border-base-content/35 has-[:checked]:border-primary has-[:checked]:bg-primary/10 has-[:focus-visible]:outline-2 has-[:focus-visible]:outline-primary"
            >
              {/* The radio covers the card: any click on it picks this one. */}
              <input
                type="radio"
                className="absolute inset-0 cursor-pointer appearance-none rounded-box"
                name={`${id}-type`}
                checked={chosen?.key === choice.key}
                onChange={() => setDraft(applyChoice(draft, choice, chosen))}
              />
              <PlatformMark platform={platformOf(choice.server || null, choice.protocol)} size={28} />
              <span className="min-w-0">
                <span className="block truncate font-medium">{choice.label}</span>
                <span className="block text-xs text-base-content/70">{choice.protocol.toUpperCase()}</span>
              </span>
            </label>
          ))}
        </div>
      </fieldset>

      <div className="grid items-start gap-3 md:grid-cols-2">
        <Field label="Name">
          <input className="input" placeholder="e.g. YouTube main" value={draft.name} onChange={(event) => set({ name: event.target.value })} />
        </Field>
        {draft.protocol !== "srt" && draft.server && (
          <Field label="Stream key" hint={`From ${chosen?.label ?? "the platform"}'s live dashboard; it is kept on this server only.`}>
            <SecretInput value={draft.input} onChange={(input) => set({ input })} />
          </Field>
        )}
        {draft.protocol !== "srt" && !draft.server && (
          <Field label="URL" hint={draft.protocol === "hls" ? "The playlist URL to upload to." : "Server and stream key in one URL."}>
            <input
              className="input font-mono"
              autoComplete="off"
              placeholder={draft.protocol === "hls" ? "https://host/path/out.m3u8" : "rtmp://host/app/stream-key"}
              value={draft.input}
              onChange={(event) => set({ input: event.target.value })}
            />
          </Field>
        )}
        {draft.protocol === "rtmp" && !draft.server && (
          <Field label="RTMP flavour" hint="Enhanced RTMP can carry H.265; older servers want legacy.">
            <select
              className="select"
              value={draft.rtmpMode}
              onChange={(event) => set({ rtmpMode: event.target.value === "enhanced" ? "enhanced" : "legacy" })}
            >
              <option value="legacy">Legacy (H.264)</option>
              <option value="enhanced">Enhanced (H.265 capable)</option>
            </select>
          </Field>
        )}
      </div>

      {draft.protocol === "srt" && (
        <>
          <div className="grid items-start gap-3 md:grid-cols-[2fr_1fr_2fr]">
            <Field label="Host">
              <input className="input" placeholder="srt.example.com" value={draft.srt.host} onChange={(event) => setSrt({ host: event.target.value })} />
            </Field>
            <Field label="Port">
              <input className="input" inputMode="numeric" placeholder="9000" value={draft.srt.port} onChange={(event) => setSrt({ port: event.target.value })} />
            </Field>
            <Field label="Stream ID">
              <input className="input font-mono" placeholder="optional" value={draft.srt.streamId} onChange={(event) => setSrt({ streamId: event.target.value })} />
            </Field>
          </div>
          <details className="group rounded-box border border-base-content/10 bg-base-100 px-3 py-2">
            <summary className="cursor-pointer text-sm font-medium">
              Encryption and options <span className="text-base-content/70">· {draft.srt.passphrase ? `AES-${Number(draft.srt.pbkeylen) * 8}` : "none"}</span>
            </summary>
            <div className="mt-3 grid items-start gap-3 md:grid-cols-3">
              <Field label="Passphrase (empty: none)">
                <input type="password" autoComplete="off" className="input" value={draft.srt.passphrase} onChange={(event) => setSrt({ passphrase: event.target.value })} />
              </Field>
              <Field label="Key length">
                <select className="select" value={draft.srt.pbkeylen} onChange={(event) => setSrt({ pbkeylen: event.target.value })}>
                  <option value="16">AES-128</option>
                  <option value="24">AES-192</option>
                  <option value="32">AES-256</option>
                </select>
              </Field>
              <Field label={<>Other options (a=b&amp;c=d)</>}>
                <input className="input font-mono" value={draft.srt.extraQuery} onChange={(event) => setSrt({ extraQuery: event.target.value })} />
              </Field>
            </div>
          </details>
        </>
      )}

      {url && (
        <p className="truncate rounded-field bg-base-100 px-3 py-1.5 font-mono text-xs text-base-content/70" aria-label="Destination URL" title="The URL the engine sends to">
          → {draft.server && draft.input ? url.split(draft.input).join("•".repeat(Math.min(12, draft.input.length))) : url}
        </p>
      )}

      <details className="rounded-box border border-base-content/10 bg-base-100 px-3 py-2" open={stored !== null && draft.profile !== ""}>
        <summary className="cursor-pointer text-sm font-medium">
          Video and audio <span className="text-base-content/70">· {videoSummary} · {audioSummary}</span>
        </summary>
        <div className="mt-3 flex flex-col gap-3">
          <div className="grid items-start gap-3 md:grid-cols-2">
            <Field label="Video">
              <select
                className="select"
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
            </Field>
            <Field label="Video codec">
              <select
                className="select"
                value={draft.codec}
                disabled={draft.profile === null}
                onChange={(event) => set({ codec: event.target.value as OutputDraft["codec"] })}
              >
                <option value="auto">Auto</option>
                <option value="h264">H.264</option>
                <option value="h265">H.265</option>
              </select>
            </Field>
          </div>
          <fieldset className="flex flex-col gap-2">
            <legend className="mb-1 text-[0.8rem] font-medium text-base-content/80">Audio</legend>
            <p className="text-xs text-base-content/70" aria-label="Audio limits">
              {chosen?.server ? chosen.label.replace(/ \(HLS\)$/, "") : getAudioPlatformLabel(platform)} over {detectAudioProtocol(url, draft.protocol).toUpperCase()} takes{" "}
              {caps.maxTracks === Infinity ? "any number of" : caps.maxTracks} track{caps.maxTracks === 1 ? "" : "s"},{" "}
              {caps.maxChannels === Infinity ? "any number of" : caps.maxChannels} channels each, codecs{" "}
              {caps.codecs === "any" ? "any" : caps.codecs.join(", ")}.
            </p>
            <select
              className="select w-full md:w-80"
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
                {Array.from({ length: choicesCount.tracks }, (_, position) => (
                  <label key={position} className="flex cursor-pointer items-center gap-2 text-sm">
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
                    {trackLabel(tracks, position)}
                  </label>
                ))}
              </div>
            )}
            {(draft.audio === "downmix" || draft.audio === "remap") && (
              <div className="flex flex-wrap gap-2">
                <select
                  className="select w-auto"
                  aria-label="Source track"
                  value={firstTrack}
                  onChange={(event) => set({ tracks: [Number(event.target.value)] })}
                >
                  {Array.from({ length: choicesCount.tracks }, (_, position) => (
                    <option key={position} value={position}>
                      {trackLabel(tracks, position)}
                    </option>
                  ))}
                </select>
                {draft.audio === "remap" &&
                  (["leftChannel", "rightChannel"] as const).map((side) => (
                    <select
                      key={side}
                      className="select w-auto"
                      aria-label={side === "leftChannel" ? "Left from channel" : "Right from channel"}
                      value={draft[side]}
                      onChange={(event) => {
                        const channel = Number(event.target.value);
                        set(side === "leftChannel" ? { leftChannel: channel } : { rightChannel: channel });
                      }}
                    >
                      {Array.from({ length: choicesCount.channels }, (_, channel) => (
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
        </div>
      </details>

      <details className="rounded-box border border-base-content/10 bg-base-100 px-3 py-2" open={Boolean(draft.monitoringUrl)}>
        <summary className="cursor-pointer text-sm font-medium">
          Watch page <span className="text-base-content/70">· {draft.monitoringUrl ? "set" : "optional"}</span>
        </summary>
        <div className="mt-3">
          <Field label="Watch page" hint="Where viewers see this stream, e.g. the YouTube watch URL. Channels links to it.">
            <input className="input" placeholder="https://youtube.com/watch?v=…" value={draft.monitoringUrl} onChange={(event) => set({ monitoringUrl: event.target.value })} />
          </Field>
        </div>
      </details>
    </SettingsForm>
  );
}

/** A secret typed into the browser: hidden until shown. */
function SecretInput(props: { value: string; onChange: (value: string) => void }) {
  const [shown, setShown] = useState(false);
  return (
    <span className="input w-full font-mono">
      <input
        type={shown ? "text" : "password"}
        autoComplete="off"
        className="grow"
        placeholder="paste the stream key"
        value={props.value}
        onChange={(event) => props.onChange(event.target.value)}
      />
      <button type="button" className="btn btn-ghost btn-xs -mr-1 font-sans" aria-pressed={shown} onClick={() => setShown(!shown)}>
        {shown ? "Hide" : "Show"}
      </button>
    </span>
  );
}

function trackLabel(tracks: readonly ProbeAudioTrack[], position: number): string {
  const track = tracks[position];
  if (!track) return `Track ${position + 1}`;
  const parts = [track.codec, track.channelLayout ?? (track.channels ? `${track.channels} ch` : null)];
  return `Track ${position + 1} (${parts.filter(Boolean).join(", ")})`;
}
