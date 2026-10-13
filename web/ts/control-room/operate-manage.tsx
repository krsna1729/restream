// Operate's editing surface: the pipeline's switches and settings, and the
// source deck's input actions. Every action reports what the engine stored.

import { useEffect, useState } from "react";

import {
  createPipelineInput,
  deletePipeline,
  deletePipelineInput,
  getPipelineSummary,
  listMediaFiles,
  startIngest,
  startRecording,
  stopIngest,
  stopRecording,
  updatePipelineInput,
} from "../core/api.js";
import type { MediaFile, PipelineSummarySnapshot } from "../core/api-types.js";
import type { ConfigPipeline, PipelineInput } from "../types.js";
import { ConfirmDelete } from "./confirm.js";
import { PipelineForm } from "./pipeline-form.js";
import { go } from "./place-link.js";
import { usePolled } from "./polled.js";

/** Recording and file ingest switches, the settings form and delete. */
export function PipelineBar(props: {
  pipeline: ConfigPipeline;
  /** Re-read the stored configuration after a change. */
  onChanged: () => void;
}) {
  const { pipeline } = props;
  const summary = usePolled<PipelineSummarySnapshot>(() => getPipelineSummary(pipeline.id));
  const recording = summary.value?.recording ?? null;
  const file = pipeline.fileIngest?.configured ? pipeline.fileIngest : null;
  const fileId = file?.id;
  const [busy, setBusy] = useState(false);
  const [editing, setEditing] = useState(false);
  const [files, setFiles] = useState<readonly MediaFile[]>([]);
  useEffect(() => {
    if (editing) void listMediaFiles().then((media) => setFiles(media?.files ?? []));
  }, [editing]);
  const act = async (action: () => Promise<unknown>, after: () => void) => {
    setBusy(true);
    try {
      await action();
    } finally {
      setBusy(false);
      after();
    }
  };
  return (
    <section className="space-y-3" aria-label="Pipeline controls">
      <div className="flex flex-wrap items-center gap-2">
        <button
          type="button"
          className={`btn btn-sm ${recording?.enabled ? "btn-error" : "btn-outline"}`}
          disabled={busy || recording === null}
          onClick={() =>
            act(
              () => (recording?.enabled ? stopRecording : startRecording)(pipeline.id),
              summary.refresh,
            )
          }
        >
          {recording?.enabled ? "Stop recording" : "Record"}
        </button>
        {recording?.enabled && (
          <span className="text-xs opacity-70">
            {recording.active ? "writing a file" : "armed: starts with the source"}
          </span>
        )}
        {file && fileId && (
          <button
            type="button"
            className="btn btn-sm btn-outline"
            disabled={busy}
            onClick={() =>
              act(() => (file.running ? stopIngest : startIngest)(fileId), props.onChanged)
            }
          >
            {file.running ? `Stop file ${file.filename}` : `Play file ${file.filename}`}
          </button>
        )}
        <button
          type="button"
          className="btn btn-sm btn-ghost"
          aria-expanded={editing}
          onClick={() => setEditing(!editing)}
        >
          Settings
        </button>
        <span className="ml-auto">
          <ConfirmDelete
            label={`Delete pipeline ${pipeline.name}`}
            question="Delete the pipeline, its inputs and destinations?"
            onDelete={async () => {
              const deleted = (await deletePipeline(pipeline.id)) !== null;
              if (deleted) go({ workspace: "pipelines", pipelineId: null });
              return deleted;
            }}
          />
        </span>
      </div>
      {editing && <PipelineForm stored={pipeline} files={files} onSaved={props.onChanged} />}
    </section>
  );
}

/** Enable, disable or delete one input. The primary input and the selected
 * one can do neither (the engine refuses). */
export function InputActions(props: {
  pipelineId: string;
  input: PipelineInput;
  onChanged: () => void;
}) {
  const { input } = props;
  if (input.role === "primary" || input.selected) return null;
  return (
    <div className="mt-2 flex flex-wrap items-center gap-2">
      <button
        type="button"
        className="btn btn-xs btn-ghost"
        onClick={async () => {
          await updatePipelineInput(props.pipelineId, input.id, { enabled: !input.enabled });
          props.onChanged();
        }}
      >
        {input.enabled ? `Disable ${input.label}` : `Enable ${input.label}`}
      </button>
      <ConfirmDelete
        label={`Delete input ${input.label}`}
        question="Delete this input?"
        onDelete={async () => {
          const deleted = (await deletePipelineInput(props.pipelineId, input.id)) !== null;
          if (deleted) props.onChanged();
          return deleted;
        }}
      />
    </div>
  );
}

/** A new backup input; its publish URLs show in the deck once created. */
export function AddInput(props: { pipelineId: string; onChanged: () => void }) {
  const [label, setLabel] = useState("");
  return (
    <form
      className="flex gap-2"
      onSubmit={async (event) => {
        event.preventDefault();
        if ((await createPipelineInput(props.pipelineId, label.trim())) !== null) {
          setLabel("");
          props.onChanged();
        }
      }}
    >
      <input
        className="input input-xs input-bordered flex-1"
        aria-label="New input label"
        placeholder="Backup input label"
        value={label}
        onChange={(event) => setLabel(event.target.value)}
      />
      <button type="submit" className="btn btn-xs" disabled={!label.trim()}>
        Add input
      </button>
    </form>
  );
}
