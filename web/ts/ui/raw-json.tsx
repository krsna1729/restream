// Engineer mode's escape hatch: the payload behind a panel, as the engine
// sent it, collapsed until asked for and copyable.

import { useState } from "react";

import { useEngineer } from "./prefs.js";

export function RawJson(props: { value: unknown; label?: string }) {
  const engineer = useEngineer();
  const [copied, setCopied] = useState(false);
  if (!engineer) return null;
  const text = JSON.stringify(props.value, null, 2) ?? "null";
  return (
    <details className="group mt-3 rounded-box border border-base-content/10 bg-base-100 text-xs">
      <summary className="cursor-pointer px-3 py-2 font-mono text-base-content/70 select-none">
        {props.label ?? "raw payload"} · {text.length.toLocaleString()} bytes
      </summary>
      <div className="relative border-t border-base-content/10">
        <button
          type="button"
          className="btn btn-xs absolute top-2 right-2"
          onClick={() => {
            void navigator.clipboard?.writeText(text).then(() => {
              setCopied(true);
              window.setTimeout(() => setCopied(false), 1500);
            });
          }}
        >
          {copied ? "copied" : "copy"}
        </button>
        <pre className="max-h-96 overflow-auto p-3 font-mono leading-relaxed">{text.length > 200_000 ? `${text.slice(0, 200_000)}\n… truncated` : text}</pre>
      </div>
    </details>
  );
}
