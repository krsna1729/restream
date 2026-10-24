import { useState } from "react";

import { createPipelineInput, updatePipelineInput } from "../core/api.js";
import { withBasePath } from "../core/base-path.js";
import type { PullHeaderPair } from "../core/api-types.js";
import type { PipelineInput, PipelineInputKind, PullAuthKind, VariantRule } from "../types.js";
import { Field } from "../ui/controls.js";
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
  choosePushAuthKind,
  inputDraft,
  inputFields,
  inputProblem,
  inputWrite,
  replaceAuth,
  replacePushAuth,
  storedPushAuthNote,
  storedAuthNote,
} from "./input-model.js";
import type { InputDraft } from "./input-model.js";
import { SettingsForm } from "./settings-form.js";

/** Create or edit an input, including write-only pull-origin or HLS publisher
 * credentials. Stored secrets are represented only by their reported state. */
export function InputForm(props: {
  pipelineId: string;
  stored: PipelineInput | null;
  /** After a save the engine accepted; the deck follows it over the state
   * stream on its own. */
  onSaved?: () => void;
}) {
  const { stored } = props;
  // What the engine last reported for this input: the save response, not the
  // pushed prop, so a second save never races the state change that follows
  // the first and mis-reads what is stored.
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
        props.onSaved?.();
        return true;
      }}
    >
      <Field label="Label">
        <input
          className="input"
          value={draft.label}
          onChange={(event) => set({ label: event.target.value })}
        />
      </Field>
      <Field label="Source">
        <select
          className="select"
          value={draft.kind}
          onChange={(event) => set({ kind: event.target.value as PipelineInputKind })}
        >
          {SOURCE_KINDS.map((kind) => (
            <option key={kind} value={kind}>
              {SOURCE_LABELS[kind]}
            </option>
          ))}
        </select>
      </Field>
      {current && current.kind === "keyed" && (
        <div className="space-y-1 text-sm">
          <p className="text-xs text-base-content/70">Publish to</p>
          {[
            current.ingestUrls?.rtmp,
            current.ingestUrls?.srt,
            current.ingestUrls?.hls,
            current.ingestUrls?.moq,
          ]
            .filter(Boolean)
            .map((url) => (
              <p key={url} className="break-all font-mono text-xs">
                {url}
              </p>
            ))}
          <p className="text-xs text-base-content/70">
            <a className="link" href={withBasePath(current.previewUrl)}>
              Preview
            </a>
          </p>
        </div>
      )}
      {draft.kind === "keyed" && (
        <div className="space-y-2">
          <Field label="HLS push authentication">
            <select
              className="select"
              value={draft.pushAuthKind}
              onChange={(event) =>
                setDraft(choosePushAuthKind(draft, event.target.value as "none" | "basic"))
              }
            >
              <option value="none">None</option>
              <option value="basic">Basic (username and password)</option>
            </select>
          </Field>
          <p className="text-xs text-base-content/70">
            Protects HLS publishing only, not RTMP, SRT, or playback. Send credentials
            separately using the publisher's HTTP Basic auth settings; publish URLs stay unchanged.
          </p>
          {draft.keepPushAuth && storedPushAuthNote(current) && (
            <div className="flex flex-wrap items-center gap-2 text-xs">
              <span role="status">{storedPushAuthNote(current)}</span>
              <button type="button" className="btn btn-xs btn-outline"
                aria-label="Replace HLS push credentials"
                onClick={() => setDraft(replacePushAuth(draft))}>Replace</button>
              <button type="button" className="btn btn-xs btn-ghost"
                aria-label="Clear HLS push credentials"
                onClick={() => setDraft(choosePushAuthKind(draft, "none"))}>Clear</button>
            </div>
          )}
          {!draft.keepPushAuth && draft.pushAuthKind === "basic" && (
            <>
              <Field label="HLS push username">
                <input className="input" autoComplete="off"
                  value={draft.pushUsername}
                  onChange={(event) => set({ pushUsername: event.target.value })} />
              </Field>
              <Field label="HLS push password">
                <input type="password" className="input"
                  autoComplete="new-password" value={draft.pushPassword}
                  onChange={(event) => set({ pushPassword: event.target.value })} />
              </Field>
            </>
          )}
        </div>
      )}
      {fields.includes("sourceUrl") && (
        <Field label="HLS source URL">
          <input
            className="input font-mono"
            inputMode="url"
            placeholder="https://origin.example/live/master.m3u8"
            value={draft.sourceUrl}
            onChange={(event) => set({ sourceUrl: event.target.value })}
          />
        </Field>
      )}
      {fields.includes("variantRule") && (
        <Field label="Rendition">
          <select
            className="select"
            value={draft.variantRule}
            onChange={(event) => set({ variantRule: event.target.value as VariantRule })}
          >
            {VARIANT_RULES.map((rule) => (
              <option key={rule} value={rule}>
                {VARIANT_LABELS[rule]}
              </option>
            ))}
          </select>
        </Field>
      )}
      {fields.includes("auth") && (
        <div className="space-y-2">
          <Field label="Authentication">
            <select
              className="select"
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
          </Field>
          {note && draft.keepAuth && (
            <div className="flex flex-wrap items-center gap-2 text-xs">
              <span role="status" className="badge badge-sm badge-ghost">
                {note}
              </span>
              <span className="text-base-content/70">Replace or clear it to change it.</span>
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
            <Field label="Token">
              <input
                type="password"
                autoComplete="off"
                className="input font-mono"
                value={draft.token}
                onChange={(event) => set({ token: event.target.value })}
              />
            </Field>
          )}
          {shown.includes("cookies") && (
            <Field label="Cookie string (copied from the origin)">
              <textarea
                rows={3}
                autoComplete="off"
                className="textarea textarea-sm font-mono"
                placeholder={AUTH_COOKIES_PLACEHOLDER}
                value={draft.cookies}
                onChange={(event) => set({ cookies: event.target.value })}
              />
              <span className="text-xs text-base-content/70">{AUTH_COOKIES_NOTE}</span>
            </Field>
          )}
          {shown.includes("username") && (
            <Field label="Username">
              <input
                autoComplete="off"
                className="input"
                value={draft.username}
                onChange={(event) => set({ username: event.target.value })}
              />
            </Field>
          )}
          {shown.includes("password") && (
            <Field label="Password">
              <input
                type="password"
                autoComplete="off"
                className="input"
                value={draft.password}
                onChange={(event) => set({ password: event.target.value })}
              />
            </Field>
          )}
          {shown.includes("headers") && (
            <div className="space-y-1">
              <span className="text-sm">Request headers</span>
              {draft.headers.map((header, index) => (
                <div key={index} className="flex flex-wrap gap-2">
                  <input
                    className="input input-xs flex-1"
                    aria-label={`Header ${index + 1} name`}
                    placeholder="Name"
                    value={header.name}
                    onChange={(event) => setHeader(index, { name: event.target.value })}
                  />
                  <input
                    className="input input-xs flex-1"
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
            <p className="text-xs text-base-content/70">{AUTH_NONE_NOTE}</p>
          )}
        </div>
      )}
    </SettingsForm>
  );
}
