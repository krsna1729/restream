import { useState } from "react";

import { createPipelineInput, updatePipelineInput } from "../core/api.js";
import { withBasePath } from "../core/base-path.js";
import type { PullHeaderPair } from "../core/api-types.js";
import type { PipelineInput, PipelineInputKind, PullAuthKind, VariantRule } from "../types.js";
import {
  AUTH_COOKIES_NOTE,
  AUTH_COOKIES_PLACEHOLDER,
  AUTH_KINDS,
  AUTH_LABELS,
  AUTH_NONE_NOTE,
  SOURCE_KINDS,
  SOURCE_LABELS,
  VARIANT_LABELS,
  VARIANT_RULES,
  authFields,
  chooseAuthKind,
  inputDraft,
  inputFields,
  inputProblem,
  inputWrite,
  replaceAuth,
  storedAuthNote,
} from "./input-model.js";
import type { InputDraft } from "./input-model.js";
import { SettingsForm } from "./settings-form.js";

/** Create a pipeline input (`stored` null) or change one: its label, whether it
 * waits for a publisher or pulls a remote HLS URL, and — for a pull input only
 * — the rendition and the credential it presents to that origin. */
export function InputForm(props: {
  pipelineId: string;
  stored: PipelineInput | null;
  onSaved: () => void;
}) {
  const { stored } = props;
  // What the engine last reported for this input: the save response, not the
  // polled prop, so a second save never races the refresh that follows the
  // first and mis-reads what is stored.
  const [current, setCurrent] = useState<PipelineInput | null>(stored);
  const [draft, setDraft] = useState<InputDraft>(() => inputDraft(stored));
  const set = (next: Partial<InputDraft>) => setDraft({ ...draft, ...next });
  const fields = inputFields(draft);
  const shown = authFields(draft);
  const note = storedAuthNote(current);
  const setHeader = (index: number, next: Partial<PullHeaderPair>) =>
    set({
      headers: draft.headers.map((header, at) => (at === index ? { ...header, ...next } : header)),
    });
  return (
    <SettingsForm
      id={stored ? `input-settings-${stored.id}` : "input-new"}
      title={current ? `Input ${current.label}` : "New input"}
      problem={inputProblem(draft)}
      onSave={async () => {
        const body = inputWrite(draft, current);
        const saved = stored
          ? await updatePipelineInput(props.pipelineId, stored.id, body)
          : await createPipelineInput(props.pipelineId, body);
        if (saved === null) return false;
        // What is on screen is what the engine stored; a kept credential stays
        // kept, so the next save does not touch it either.
        setCurrent(saved.input);
        setDraft(inputDraft(saved.input));
        props.onSaved();
        return true;
      }}
    >
      <label className="form-control">
        <span className="label-text">Label</span>
        <input
          className="input input-sm input-bordered"
          value={draft.label}
          onChange={(event) => set({ label: event.target.value })}
        />
      </label>
      <label className="form-control">
        <span className="label-text">Source</span>
        <select
          className="select select-sm select-bordered"
          value={draft.kind}
          onChange={(event) => set({ kind: event.target.value as PipelineInputKind })}
        >
          {SOURCE_KINDS.map((kind) => (
            <option key={kind} value={kind}>
              {SOURCE_LABELS[kind]}
            </option>
          ))}
        </select>
      </label>
      {current && current.kind === "keyed" && (
        <div className="space-y-1 text-sm">
          <p className="text-xs opacity-70">Publish to</p>
          {[current.ingestUrls?.rtmp, current.ingestUrls?.srt, current.ingestUrls?.hls]
            .filter(Boolean)
            .map((url) => (
              <p key={url} className="break-all font-mono text-xs">
                {url}
              </p>
            ))}
          <p className="text-xs opacity-70">
            <a className="link" href={withBasePath(current.previewUrl)}>
              Preview
            </a>
          </p>
        </div>
      )}
      {fields.includes("sourceUrl") && (
        <label className="form-control">
          <span className="label-text">HLS source URL</span>
          <input
            className="input input-sm input-bordered font-mono"
            inputMode="url"
            placeholder="https://origin.example/live/master.m3u8"
            value={draft.sourceUrl}
            onChange={(event) => set({ sourceUrl: event.target.value })}
          />
        </label>
      )}
      {fields.includes("variantRule") && (
        <label className="form-control">
          <span className="label-text">Rendition</span>
          <select
            className="select select-sm select-bordered"
            value={draft.variantRule}
            onChange={(event) => set({ variantRule: event.target.value as VariantRule })}
          >
            {VARIANT_RULES.map((rule) => (
              <option key={rule} value={rule}>
                {VARIANT_LABELS[rule]}
              </option>
            ))}
          </select>
        </label>
      )}
      {fields.includes("auth") && (
        <div className="space-y-2">
          <label className="form-control">
            <span className="label-text">Authentication</span>
            <select
              className="select select-sm select-bordered"
              value={draft.authKind}
              onChange={(event) =>
                setDraft(chooseAuthKind(draft, event.target.value as PullAuthKind))
              }
            >
              {AUTH_KINDS.map((kind) => (
                <option key={kind} value={kind}>
                  {AUTH_LABELS[kind]}
                </option>
              ))}
            </select>
          </label>
          {note && draft.keepAuth && (
            <div className="flex flex-wrap items-center gap-2 text-xs">
              <span role="status" className="badge badge-sm badge-ghost">
                {note}
              </span>
              <span className="opacity-70">Replace or clear it to change it.</span>
              <button
                type="button"
                className="btn btn-xs btn-outline"
                onClick={() => setDraft(replaceAuth(draft))}
              >
                Replace
              </button>
              <button
                type="button"
                className="btn btn-xs btn-ghost"
                onClick={() => setDraft(chooseAuthKind(draft, "none"))}
              >
                Clear
              </button>
            </div>
          )}
          {shown.includes("token") && (
            <label className="form-control">
              <span className="label-text">Token</span>
              <input
                type="password"
                autoComplete="off"
                className="input input-sm input-bordered font-mono"
                value={draft.token}
                onChange={(event) => set({ token: event.target.value })}
              />
            </label>
          )}
          {shown.includes("cookies") && (
            <label className="form-control">
              <span className="label-text">Cookie string (copied from the origin)</span>
              <textarea
                rows={3}
                autoComplete="off"
                className="textarea textarea-sm textarea-bordered font-mono"
                placeholder={AUTH_COOKIES_PLACEHOLDER}
                value={draft.cookies}
                onChange={(event) => set({ cookies: event.target.value })}
              />
              <span className="text-xs opacity-70">{AUTH_COOKIES_NOTE}</span>
            </label>
          )}
          {shown.includes("username") && (
            <label className="form-control">
              <span className="label-text">Username</span>
              <input
                autoComplete="off"
                className="input input-sm input-bordered"
                value={draft.username}
                onChange={(event) => set({ username: event.target.value })}
              />
            </label>
          )}
          {shown.includes("password") && (
            <label className="form-control">
              <span className="label-text">Password</span>
              <input
                type="password"
                autoComplete="off"
                className="input input-sm input-bordered"
                value={draft.password}
                onChange={(event) => set({ password: event.target.value })}
              />
            </label>
          )}
          {shown.includes("headers") && (
            <div className="space-y-1">
              <span className="label-text">Request headers</span>
              {draft.headers.map((header, index) => (
                <div key={index} className="flex flex-wrap gap-2">
                  <input
                    className="input input-xs input-bordered flex-1"
                    aria-label={`Header ${index + 1} name`}
                    placeholder="Name"
                    value={header.name}
                    onChange={(event) => setHeader(index, { name: event.target.value })}
                  />
                  <input
                    className="input input-xs input-bordered flex-1"
                    aria-label={`Header ${index + 1} value`}
                    placeholder="Value"
                    value={header.value}
                    onChange={(event) => setHeader(index, { value: event.target.value })}
                  />
                  <button
                    type="button"
                    className="btn btn-xs btn-ghost"
                    aria-label={`Remove header ${index + 1}`}
                    onClick={() =>
                      set({ headers: draft.headers.filter((_, at) => at !== index) })
                    }
                  >
                    Remove
                  </button>
                </div>
              ))}
              <button
                type="button"
                className="btn btn-xs btn-outline"
                onClick={() => set({ headers: [...draft.headers, { name: "", value: "" }] })}
              >
                Add header
              </button>
            </div>
          )}
          {draft.authKind === "none" && !draft.keepAuth && (
            <p className="text-xs opacity-70">{AUTH_NONE_NOTE}</p>
          )}
        </div>
      )}
    </SettingsForm>
  );
}
