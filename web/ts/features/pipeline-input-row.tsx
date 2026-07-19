import { useState } from "react";

import type { PipelineInput } from "../types.js";
import type { PipelineInputsPanelActions } from "./pipeline-inputs-contract.js";
import {
  formatPipelineInputBytes,
  pipelineInputStatusLabel,
} from "./pipeline-inputs-view-model.js";

interface PipelineInputRowProps {
  readonly actions: PipelineInputsPanelActions;
  readonly busy: boolean;
  readonly input: PipelineInput;
  readonly pipelineId: string;
  readonly runMutation: (
    inputId: string,
    mutation: () => Promise<unknown>,
  ) => Promise<void>;
}

function inputTone(input: PipelineInput): string {
  if (input.runtime.forwardingState === "active") return "text-success";
  if (input.runtime.forwardingState === "awaiting_keyframe")
    return "text-warning";
  return "text-base-content/60";
}

export function PipelineInputRow({
  actions,
  busy,
  input,
  pipelineId,
  runMutation,
}: PipelineInputRowProps): React.JSX.Element {
  const [editing, setEditing] = useState(false);
  const [labelDraft, setLabelDraft] = useState(input.label);
  const [deleteConfirm, setDeleteConfirm] = useState(false);
  const deletable = input.role !== "primary" && !input.selected;
  const disableable = input.role !== "primary" && !input.selected;

  const saveLabel = async (): Promise<void> => {
    const label = labelDraft.trim();
    if (!label || label === input.label) {
      setEditing(false);
      return;
    }
    await runMutation(input.id, async () => {
      const response = await actions.updateInput(pipelineId, input.id, {
        label,
      });
      if (response) setEditing(false);
    });
  };

  return (
    <article className="py-3">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          {editing ? (
            <div className="flex flex-wrap items-center gap-2">
              <input
                aria-label={`Rename ${input.label}`}
                autoFocus
                className="input input-bordered input-xs min-w-48 flex-1"
                maxLength={128}
                onChange={(event) => setLabelDraft(event.currentTarget.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") void saveLabel();
                  if (event.key === "Escape") setEditing(false);
                }}
                value={labelDraft}
              />
              <button
                aria-label={`Save input name for ${input.label}`}
                className="btn btn-xs btn-accent"
                disabled={busy}
                onClick={() => void saveLabel()}
                type="button"
              >
                Save
              </button>
              <button
                aria-label={`Cancel input rename for ${input.label}`}
                className="btn btn-xs btn-ghost"
                disabled={busy}
                onClick={() => setEditing(false)}
                type="button"
              >
                Cancel
              </button>
            </div>
          ) : (
            <div className="flex flex-wrap items-center gap-2">
              <h4 className="min-w-0 truncate text-sm font-semibold">
                {input.label}
              </h4>
              <span className="badge badge-sm badge-outline">
                {input.selected ? "Selected" : "Standby"}
              </span>
              <span className={`${inputTone(input)} text-xs font-medium`}>
                {pipelineInputStatusLabel(input)}
              </span>
            </div>
          )}
          <p className="text-base-content/55 mt-1 text-xs">
            {input.runtime.protocol?.toUpperCase() ?? "No publisher"}
            {input.runtime.remoteAddr ? ` · ${input.runtime.remoteAddr}` : ""}
            {input.runtime.connected
              ? ` · ${formatPipelineInputBytes(input.runtime.bytesReceived)} received`
              : ""}
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-1.5">
          {!input.selected ? (
            <button
              aria-label={`Promote ${input.label}`}
              className="btn btn-xs btn-accent"
              disabled={!input.enabled || busy}
              onClick={() =>
                void runMutation(input.id, () =>
                  actions.promoteInput(pipelineId, input.id),
                )
              }
              type="button"
            >
              Promote
            </button>
          ) : null}
          <button
            aria-label={`Rename ${input.label}`}
            className="btn btn-xs btn-outline"
            disabled={busy}
            onClick={() => {
              setEditing(true);
              setLabelDraft(input.label);
            }}
            type="button"
          >
            Rename
          </button>
          {disableable ? (
            <button
              aria-label={`${input.enabled ? "Disable" : "Enable"} ${input.label}`}
              className="btn btn-xs btn-outline"
              disabled={busy}
              onClick={() =>
                void runMutation(input.id, () =>
                  actions.updateInput(pipelineId, input.id, {
                    enabled: !input.enabled,
                  }),
                )
              }
              type="button"
            >
              {input.enabled ? "Disable" : "Enable"}
            </button>
          ) : null}
          {deletable ? (
            deleteConfirm ? (
              <>
                <button
                  aria-label={`Confirm delete ${input.label}`}
                  className="btn btn-xs btn-error"
                  disabled={busy}
                  onClick={() =>
                    void runMutation(input.id, async () => {
                      const response = await actions.deleteInput(
                        pipelineId,
                        input.id,
                      );
                      if (response) setDeleteConfirm(false);
                    })
                  }
                  type="button"
                >
                  Delete
                </button>
                <button
                  aria-label={`Cancel delete ${input.label}`}
                  className="btn btn-xs btn-ghost"
                  disabled={busy}
                  onClick={() => setDeleteConfirm(false)}
                  type="button"
                >
                  Cancel
                </button>
              </>
            ) : (
              <button
                aria-label={`Delete ${input.label}`}
                className="btn btn-xs btn-ghost text-error"
                disabled={busy}
                onClick={() => setDeleteConfirm(true)}
                type="button"
              >
                Delete
              </button>
            )
          ) : null}
        </div>
      </div>

      <div className="mt-2 grid gap-2 lg:grid-cols-3">
        <div className="bg-base-200/45 min-w-0 rounded-md p-2">
          <div className="text-base-content/55 text-[0.7rem]">Stream key</div>
          <div className="mt-1 flex min-w-0 items-start gap-2">
            <code className="min-w-0 flex-1 overflow-x-auto text-xs whitespace-nowrap">
              {input.streamKey}
            </code>
            <button
              aria-label={`Copy stream key for ${input.label}`}
              className="btn btn-xs btn-ghost"
              onClick={() => void actions.copyValue(input.streamKey)}
              type="button"
            >
              Copy
            </button>
          </div>
        </div>
        {(["rtmp", "srt"] as const).map((protocol) => {
          const url = input.ingestUrls[protocol];
          return (
            <div
              className="bg-base-200/45 min-w-0 rounded-md p-2"
              key={protocol}
            >
              <div className="text-base-content/55 text-[0.7rem] uppercase">
                {protocol}
              </div>
              <div className="mt-1 flex min-w-0 items-start gap-2">
                <code
                  className="min-w-0 flex-1 truncate text-xs"
                  title={url ?? ""}
                >
                  {url ?? "Unavailable"}
                </code>
                <button
                  aria-label={`Copy ${protocol.toUpperCase()} ingest URL for ${input.label}`}
                  className="btn btn-xs btn-ghost"
                  disabled={!url}
                  onClick={() => {
                    if (url) void actions.copyValue(url);
                  }}
                  type="button"
                >
                  Copy
                </button>
              </div>
            </div>
          );
        })}
      </div>
    </article>
  );
}
