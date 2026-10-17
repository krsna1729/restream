// Operate's editing surface: the pipeline's switches and settings, and the
// source deck's input actions. Every action reports what the engine stored.

import { useEffect, useState } from "react";

import {
  deletePipeline,
  deletePipelineInput,
  getPipelineFileIngest,
  getPipelineSummary,
  listMediaFiles,
  startIngest,
  startRecording,
  stopIngest,
  stopRecording,
  updatePipelineInput,
} from "../core/api.js";
import type {
  MediaFile,
  PipelineFileIngestConfig,
  PipelineSummarySnapshot,
} from "../core/api-types.js";
import type { ConfigPipeline, PipelineInput } from "../types.js";
import { ConfirmDelete } from "./confirm.js";
import { InputForm } from "./input-form.js";
import { PipelineForm } from "./pipeline-form.js";
import { usePolled } from "./polled.js";
import { useGo } from "./router.js";

/** Recording and file ingest switches, the settings form and delete. */
export function PipelineBar(props: {
  pipeline: ConfigPipeline;
  /** Destinations are running, so the key cannot be rotated. */
  keyLocked: boolean;
  /** Re-read the stored configuration after a change. */
  onChanged: () => void;
}) {
  const { pipeline } = props;
  const go = useGo();
  const summary = usePolled<PipelineSummarySnapshot>(
    ["pipeline-summary", pipeline.id],
    () => getPipelineSummary(pipeline.id),
  );
  const recording = summary.value?.recording ?? null;
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
        {pipeline.fileIngest?.configured && (
          <FileSwitch key={pipeline.fileIngest.id} pipelineId={pipeline.id} busy={busy} act={act} />
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
      {editing && (
        <PipelineForm
          stored={pipeline}
          files={files}
          keyLocked={props.keyLocked}
          onSaved={props.onChanged}
        />
      )}
    </section>
  );
}

/** Play or stop the configured file. Whether it runs is read from the
 * engine every poll: a file can end, fail or be started elsewhere. */
function FileSwitch(props: {
  pipelineId: string;
  busy: boolean;
  act: (action: () => Promise<unknown>, after: () => void) => Promise<void>;
}) {
  const state = usePolled<PipelineFileIngestConfig>(
    ["pipeline-file-ingest", props.pipelineId],
    () => getPipelineFileIngest(props.pipelineId),
  );
  const file = state.value;
  const fileId = file?.configured ? file.id : undefined;
  if (!file || !fileId) return null;
  return (
    <button
      type="button"
      className="btn btn-sm btn-outline"
      disabled={props.busy}
      onClick={() => props.act(() => (file.running ? stopIngest : startIngest)(fileId), state.refresh)}
    >
      {file.running ? `Stop file ${file.filename}` : `Play file ${file.filename}`}
    </button>
  );
}

/** Enable, disable or delete one input. The primary input and the selected
 * one can do neither (the engine refuses). The deck follows the change over
 * the state stream. */
export function InputActions(props: { pipelineId: string; input: PipelineInput }) {
  const { input } = props;
  if (input.role === "primary" || input.selected) return null;
  return (
    <div className="mt-2 flex flex-wrap items-center gap-2">
      <button
        type="button"
        className="btn btn-xs btn-ghost"
        onClick={() =>
          void updatePipelineInput(props.pipelineId, input.id, { enabled: !input.enabled })
        }
      >
        {input.enabled ? `Disable ${input.label}` : `Enable ${input.label}`}
      </button>
      <ConfirmDelete
        label={`Delete input ${input.label}`}
        question="Delete this input?"
        onDelete={async () =>
          (await deletePipelineInput(props.pipelineId, input.id)) !== null
        }
      />
    </div>
  );
}

/** Change one input: its label, the source it receives media from, and for a
 * pull input its rendition and credential. Open to every input. */
export function EditInput(props: { pipelineId: string; input: PipelineInput }) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button
        type="button"
        className="btn btn-xs btn-ghost mt-2"
        aria-expanded={open}
        onClick={() => setOpen(!open)}
      >
        {open ? "Close" : `Edit ${props.input.label}`}
      </button>
      {open && (
        <InputForm pipelineId={props.pipelineId} stored={props.input} />
      )}
    </>
  );
}

/** A new input: one that waits for a publisher, or one that pulls a remote HLS
 * URL. Its source line shows in the deck once created. */
export function AddInput(props: { pipelineId: string }) {
  const [open, setOpen] = useState(false);
  if (!open) {
    return (
      <button type="button" className="btn btn-xs btn-outline" onClick={() => setOpen(true)}>
        Add input
      </button>
    );
  }
  return (
    <InputForm
      pipelineId={props.pipelineId}
      stored={null}
      onSaved={() => setOpen(false)}
    />
  );
}
