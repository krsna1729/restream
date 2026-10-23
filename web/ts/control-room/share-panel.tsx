// Share: public watch links to this pipeline's live preview, for an admin
// to make, copy (link or embed code) and revoke. Anyone with a live link
// watches without signing in; revoking stops every player at once.

import { useCallback, useEffect, useState } from "react";

import { createShareLink, listShareLinks, revokeShareLink } from "../core/api.js";
import type { ShareLink } from "../core/api-types.js";
import { EmptyState, Panel } from "../ui/panel.js";
import { StatusBadge } from "../ui/tone.js";
import { ConfirmDelete } from "./confirm.js";
import { LIFETIMES, embedSnippet, linkState, watchUrl } from "./share-model.js";
import { useNow } from "./store.js";
import { absoluteTime } from "./when.js";


function CopyButton(props: { text: string; label: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      className="btn btn-ghost btn-xs"
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(props.text);
          setCopied(true);
          window.setTimeout(() => setCopied(false), 1500);
        } catch {
          window.prompt("Copy this:", props.text);
        }
      }}
    >
      {copied ? "Copied" : props.label}
    </button>
  );
}

export function SharePanel(props: { pipelineId: string; pipelineName: string }) {
  const now = useNow(60_000);
  const [links, setLinks] = useState<ShareLink[] | null>(null);
  const [label, setLabel] = useState("");
  const [lifetime, setLifetime] = useState<number | null>(24);
  const reload = useCallback(() => void listShareLinks(props.pipelineId).then(setLinks), [props.pipelineId]);
  useEffect(reload, [reload]);

  return (
    <Panel id={`share-${props.pipelineId}`} title="Share" subtitle="Public watch links: no sign-in, this pipeline only">
      <form
        className="flex flex-wrap items-end gap-2"
        onSubmit={async (event) => {
          event.preventDefault();
          if (!label.trim()) return;
          if (await createShareLink(props.pipelineId, label.trim(), lifetime)) {
            setLabel("");
            reload();
          }
        }}
      >
        <label className="flex min-w-48 flex-1 flex-col gap-1 text-sm">
          <span className="kicker">Label</span>
          <input className="input input-sm w-full" aria-label="Share link label" placeholder="Website embed" value={label} onChange={(e) => setLabel(e.target.value)} />
        </label>
        <label className="flex w-40 flex-col gap-1 text-sm">
          <span className="kicker">Works for</span>
          <select
            className="select select-sm w-full"
            aria-label="Link lifetime"
            value={lifetime === null ? "" : String(lifetime)}
            onChange={(e) => setLifetime(e.target.value === "" ? null : Number(e.target.value))}
          >
            {LIFETIMES.map((choice) => (
              <option key={choice.label} value={choice.hours === null ? "" : String(choice.hours)}>
                {choice.label}
              </option>
            ))}
          </select>
        </label>
        <button type="submit" className="btn btn-primary btn-sm" disabled={!label.trim()}>
          Create link
        </button>
      </form>
      {links === null ? null : links.length === 0 ? (
        <EmptyState title="No links yet." detail="A link plays this pipeline's preview in any browser, or embedded on a site." />
      ) : (
        <ul className="mt-3 flex flex-col gap-2" aria-label="Share links">
          {links.map((link) => {
            const url = watchUrl(window.location.href, link.token);
            const state = linkState(link, now);
            return (
              <li key={link.id} aria-label={`Share link ${link.label}`} className="flex min-w-0 flex-col gap-2 rounded-box border border-base-content/10 bg-base-100 p-3 md:flex-row md:items-center">
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="truncate font-semibold">{link.label}</span>
                    <StatusBadge tone={state === "expired" ? "neutral" : state === "expiring" ? "warn" : "ok"}>
                      {state === "expired" ? "Expired" : link.expiresAtMs === null ? "Until revoked" : `Until ${absoluteTime(link.expiresAtMs, Date.now())}`}
                    </StatusBadge>
                    <span className="tabular text-xs text-base-content/65">{link.views.toLocaleString()} views</span>
                  </div>
                  <code className="mt-1 block truncate text-xs text-base-content/70">{url}</code>
                </div>
                <div className="flex flex-wrap items-center gap-1">
                  <CopyButton text={url} label="Copy link" />
                  <CopyButton text={embedSnippet(url, props.pipelineName)} label="Copy embed" />
                  <a className="btn btn-ghost btn-xs" href={url} target="_blank" rel="noreferrer">
                    Open
                  </a>
                  <ConfirmDelete
                    label={`Revoke ${link.label}`}
                    question="Revoke? Players stop at once."
                    verb="Revoke"
                    onDelete={async () => {
                      const revoked = await revokeShareLink(link.id);
                      if (revoked) reload();
                      return revoked;
                    }}
                  />
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </Panel>
  );
}
