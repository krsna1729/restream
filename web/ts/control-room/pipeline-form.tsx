import { useState } from "react";

import { createPipeline, updatePipeline } from "../core/api.js";
import type { MediaFile } from "../core/api-types.js";
import type { ConfigPipeline } from "../types.js";
import { Field } from "../ui/controls.js";
import { moveQueued, pipelineDraft, pipelineProblem, pipelineWrite } from "./manage-model.js";
import type { PipelineDraft } from "./manage-model.js";
import { SettingsForm } from "./settings-form.js";
import { useMediaDurations } from "./media-durations.js";
import { formatDuration, playlistRuntime, runtimeWords } from "./media-model.js";

/** Create a pipeline (`stored` null) or change one: name, stream key, live or
 * file source, and its SRT ingest override. */
export function PipelineForm(props: {
  stored: ConfigPipeline | null;
  /** Media files a file source can ingest. */
  files: readonly MediaFile[];
  /** Destinations are running: a new key would cut the encoder at its next
   * reconnect, so it is not offered (as in the classic editor). */
  keyLocked: boolean;
  onSaved: (pipeline: ConfigPipeline) => void;
  /** Fields a new pipeline starts with (Media's "use as source"). */
  initial?: Partial<PipelineDraft>;
}) {
  const { stored } = props;
  const [draft, setDraft] = useState<PipelineDraft>(() => ({
    ...pipelineDraft(stored),
    ...props.initial,
  }));
  const set = (next: Partial<PipelineDraft>) => setDraft({ ...draft, ...next });
  const encrypted = draft.srtMode === "encrypted";
  return (
    <SettingsForm
      id={stored ? `pipeline-settings-${stored.id}` : "pipeline-new"}
      title={stored ? "Pipeline settings" : "New pipeline"}
      problem={pipelineProblem(draft)}
      onSave={async () => {
        const body = pipelineWrite({ ...draft, rotateKey: draft.rotateKey && !props.keyLocked }, stored);
        const saved = stored ? await updatePipeline(stored.id, body) : await createPipeline(body);
        if (saved === null) return false;
        // The form now shows what the engine stored (a rotated key is not
        // rotated again on the next save).
        setDraft(pipelineDraft(saved.pipeline));
        props.onSaved(saved.pipeline);
        return true;
      }}
    >
      <Field label="Name">
        <input
          className="input"
          value={draft.name}
          onChange={(event) => set({ name: event.target.value })}
        />
      </Field>
      {stored ? (
        <div className="space-y-1 text-sm">
          <p className="text-xs text-base-content/70">Publish to</p>
          {stored.ingestUrls?.rtmp && <p className="break-all font-mono text-xs">{stored.ingestUrls.rtmp}</p>}
          {stored.ingestUrls?.srt && <p className="break-all font-mono text-xs">{stored.ingestUrls.srt}</p>}
          <label className="label cursor-pointer justify-start gap-2">
            <input
              type="checkbox"
              className="checkbox checkbox-sm"
              checked={draft.rotateKey && !props.keyLocked}
              disabled={props.keyLocked}
              onChange={(event) => set({ rotateKey: event.target.checked })}
            />
            <span className="text-sm">Issue a new stream key (publishers must switch to it)</span>
          </label>
          {props.keyLocked && (
            <p className="text-xs text-base-content/70">Stop the destinations first: the encoder would lose the key at its next reconnect.</p>
          )}
        </div>
      ) : (
        <Field label="Stream key (empty: the engine makes one)">
          <input
            className="input font-mono"
            autoComplete="off"
            value={draft.streamKey}
            onChange={(event) => set({ streamKey: event.target.value })}
          />
        </Field>
      )}
      <Field label="Source">
        <select
          className="select"
          value={draft.source}
          onChange={(event) => set({ source: event.target.value === "file" ? "file" : "live" })}
        >
          <option value="live">A live publisher (RTMP or SRT)</option>
          <option value="file">A media file</option>
        </select>
      </Field>
      {draft.source === "file" && (
        <div className="grid items-start gap-3 md:grid-cols-2">
          <Field label={draft.queue.length > 0 ? "First file" : "File"} className="md:col-span-2">
            <select
              className="select"
              value={draft.filename}
              onChange={(event) => set({ filename: event.target.value })}
            >
              <option value="">Choose a file</option>
              {props.files.map((file) => (
                <option key={file.name} value={file.name}>
                  {file.name}
                </option>
              ))}
            </select>
          </Field>
          <Playlist first={draft.filename} queue={draft.queue} files={props.files} onChange={(queue) => set({ queue })} />
          <Field label="Start at (HH:MM:SS, empty: the start)">
            <input
              className="input"
              value={draft.startTime}
              onChange={(event) => set({ startTime: event.target.value })}
            />
          </Field>
          <Field label="Keyframe interval (s, Live Optimized)">
            <input
              type="number"
              min={1}
              className="input"
              disabled={!draft.liveOptimized}
              value={draft.targetGopSeconds}
              onChange={(event) => set({ targetGopSeconds: Number(event.target.value) })}
            />
          </Field>
          <label className="label cursor-pointer justify-start gap-2">
            <input
              type="checkbox"
              className="checkbox checkbox-sm"
              checked={draft.loop}
              onChange={(event) => set({ loop: event.target.checked })}
            />
            <span className="text-sm">{draft.queue.length > 0 ? "Loop the playlist" : "Loop"}</span>
          </label>
          <label className="label cursor-pointer justify-start gap-2">
            <input
              type="checkbox"
              className="checkbox checkbox-sm"
              checked={draft.liveOptimized}
              onChange={(event) => set({ liveOptimized: event.target.checked })}
            />
            <span className="text-sm">Live Optimized (re-encode to the keyframe interval)</span>
          </label>
        </div>
      )}
      <div className="grid items-start gap-3 md:grid-cols-2">
        <Field label="SRT ingest encryption">
          <select
            className="select"
            value={draft.srtMode}
            onChange={(event) =>
              set({ srtMode: event.target.value as PipelineDraft["srtMode"] })
            }
          >
            <option value="inherit">as the engine settings</option>
            <option value="plaintext">plaintext</option>
            <option value="encrypted">encrypted</option>
          </select>
        </Field>
        <Field label="SRT receive latency (ms, empty: engine setting)">
          <input
            type="number"
            min={20}
            max={8000}
            className="input"
            value={draft.srtLatencyMs}
            onChange={(event) => set({ srtLatencyMs: event.target.value })}
          />
        </Field>
        {encrypted && (
          <>
            <Field label="SRT passphrase (10 to 79 bytes)">
              <input
                type="password"
                autoComplete="off"
                className="input"
                value={draft.srtPassphrase}
                onChange={(event) => set({ srtPassphrase: event.target.value })}
              />
            </Field>
            <Field label="SRT key length">
              <select
                className="select"
                value={draft.srtPbkeylen}
                onChange={(event) =>
                  set({ srtPbkeylen: Number(event.target.value) as PipelineDraft["srtPbkeylen"] })
                }
              >
                <option value={16}>AES-128</option>
                <option value={24}>AES-192</option>
                <option value={32}>AES-256</option>
              </select>
            </Field>
          </>
        )}
      </div>
    </SettingsForm>
  );
}

/** The files that play after the first, in order: add from the library,
 * move, remove. Files should share codecs (or use Live Optimized). */
function Playlist(props: { first: string; queue: readonly string[]; files: readonly MediaFile[]; onChange: (queue: string[]) => void }) {
  const [adding, setAdding] = useState("");
  const durations = useMediaDurations([props.first, ...props.queue]);
  const runtime = playlistRuntime(durations);
  return (
    <div className="flex flex-col gap-2 md:col-span-2" role="group" aria-label="Playlist">
      <span className="text-sm">
        Then play
        {props.queue.length > 0 && <span className="text-base-content/60 tabular"> · the whole playlist {runtimeWords(runtime)}</span>}
      </span>
      {props.queue.length === 0 ? (
        <p className="text-xs text-base-content/65">Nothing: the file plays alone. Add files to make a playlist.</p>
      ) : (
        <ol className="flex flex-col gap-1">
          {props.queue.map((name, index) => (
            <li key={`${index}-${name}`} className="flex min-w-0 items-center gap-2 rounded-field border border-base-content/10 bg-base-100 px-2 py-1 text-sm">
              <span className="tabular w-6 shrink-0 text-base-content/60">{index + 2}.</span>
              <span className="min-w-0 flex-1 truncate">{name}</span>
              <span className="tabular shrink-0 text-xs text-base-content/60">{formatDuration(durations[index + 1])}</span>
              <button type="button" className="btn btn-ghost btn-xs" aria-label={`Move ${name} up`} disabled={index === 0} onClick={() => props.onChange(moveQueued(props.queue, index, -1))}>
                ↑
              </button>
              <button type="button" className="btn btn-ghost btn-xs" aria-label={`Move ${name} down`} disabled={index === props.queue.length - 1} onClick={() => props.onChange(moveQueued(props.queue, index, 1))}>
                ↓
              </button>
              <button type="button" className="btn btn-ghost btn-xs" aria-label={`Remove ${name}`} onClick={() => props.onChange(props.queue.filter((_, at) => at !== index))}>
                ✕
              </button>
            </li>
          ))}
        </ol>
      )}
      <div className="flex gap-2">
        <select className="select min-w-0 flex-1" aria-label="Add to playlist" value={adding} onChange={(event) => setAdding(event.target.value)}>
          <option value="">Add a file…</option>
          {props.files.map((file) => (
            <option key={file.name} value={file.name}>
              {file.name}
            </option>
          ))}
        </select>
        <button
          type="button"
          className="btn btn-sm"
          disabled={!adding}
          onClick={() => {
            props.onChange([...props.queue, adding]);
            setAdding("");
          }}
        >
          Add
        </button>
      </div>
    </div>
  );
}
