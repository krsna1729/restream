import { useState } from "react";

import { createPipeline, updatePipeline } from "../core/api.js";
import type { MediaFile } from "../core/api-types.js";
import type { ConfigPipeline } from "../types.js";
import { pipelineDraft, pipelineProblem, pipelineWrite } from "./manage-model.js";
import type { PipelineDraft } from "./manage-model.js";
import { SettingsForm } from "./settings-form.js";

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
      <label className="form-control">
        <span className="label-text">Name</span>
        <input
          className="input input-sm input-bordered"
          value={draft.name}
          onChange={(event) => set({ name: event.target.value })}
        />
      </label>
      {stored ? (
        <div className="space-y-1 text-sm">
          <p className="text-xs opacity-70">Publish to</p>
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
            <span className="label-text">Issue a new stream key (publishers must switch to it)</span>
          </label>
          {props.keyLocked && (
            <p className="text-xs opacity-70">Stop the destinations first: the encoder would lose the key at its next reconnect.</p>
          )}
        </div>
      ) : (
        <label className="form-control">
          <span className="label-text">Stream key (empty: the engine makes one)</span>
          <input
            className="input input-sm input-bordered font-mono"
            autoComplete="off"
            value={draft.streamKey}
            onChange={(event) => set({ streamKey: event.target.value })}
          />
        </label>
      )}
      <label className="form-control">
        <span className="label-text">Source</span>
        <select
          className="select select-sm select-bordered"
          value={draft.source}
          onChange={(event) => set({ source: event.target.value === "file" ? "file" : "live" })}
        >
          <option value="live">A live publisher (RTMP or SRT)</option>
          <option value="file">A media file</option>
        </select>
      </label>
      {draft.source === "file" && (
        <div className="grid gap-2 md:grid-cols-2">
          <label className="form-control md:col-span-2">
            <span className="label-text">File</span>
            <select
              className="select select-sm select-bordered"
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
          </label>
          <label className="form-control">
            <span className="label-text">Start at (HH:MM:SS, empty: the start)</span>
            <input
              className="input input-sm input-bordered"
              value={draft.startTime}
              onChange={(event) => set({ startTime: event.target.value })}
            />
          </label>
          <label className="form-control">
            <span className="label-text">Keyframe interval (s, Live Optimized)</span>
            <input
              type="number"
              min={1}
              className="input input-sm input-bordered"
              disabled={!draft.liveOptimized}
              value={draft.targetGopSeconds}
              onChange={(event) => set({ targetGopSeconds: Number(event.target.value) })}
            />
          </label>
          <label className="label cursor-pointer justify-start gap-2">
            <input
              type="checkbox"
              className="checkbox checkbox-sm"
              checked={draft.loop}
              onChange={(event) => set({ loop: event.target.checked })}
            />
            <span className="label-text">Loop</span>
          </label>
          <label className="label cursor-pointer justify-start gap-2">
            <input
              type="checkbox"
              className="checkbox checkbox-sm"
              checked={draft.liveOptimized}
              onChange={(event) => set({ liveOptimized: event.target.checked })}
            />
            <span className="label-text">Live Optimized (re-encode to the keyframe interval)</span>
          </label>
        </div>
      )}
      <div className="grid gap-2 md:grid-cols-2">
        <label className="form-control">
          <span className="label-text">SRT ingest encryption</span>
          <select
            className="select select-sm select-bordered"
            value={draft.srtMode}
            onChange={(event) =>
              set({ srtMode: event.target.value as PipelineDraft["srtMode"] })
            }
          >
            <option value="inherit">as the engine settings</option>
            <option value="plaintext">plaintext</option>
            <option value="encrypted">encrypted</option>
          </select>
        </label>
        <label className="form-control">
          <span className="label-text">SRT receive latency (ms, empty: engine setting)</span>
          <input
            type="number"
            min={20}
            max={8000}
            className="input input-sm input-bordered"
            value={draft.srtLatencyMs}
            onChange={(event) => set({ srtLatencyMs: event.target.value })}
          />
        </label>
        {encrypted && (
          <>
            <label className="form-control">
              <span className="label-text">SRT passphrase (10 to 79 bytes)</span>
              <input
                type="password"
                autoComplete="off"
                className="input input-sm input-bordered"
                value={draft.srtPassphrase}
                onChange={(event) => set({ srtPassphrase: event.target.value })}
              />
            </label>
            <label className="form-control">
              <span className="label-text">SRT key length</span>
              <select
                className="select select-sm select-bordered"
                value={draft.srtPbkeylen}
                onChange={(event) =>
                  set({ srtPbkeylen: Number(event.target.value) as PipelineDraft["srtPbkeylen"] })
                }
              >
                <option value={16}>AES-128</option>
                <option value={24}>AES-192</option>
                <option value={32}>AES-256</option>
              </select>
            </label>
          </>
        )}
      </div>
    </SettingsForm>
  );
}
