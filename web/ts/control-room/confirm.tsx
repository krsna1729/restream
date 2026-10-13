import { useState } from "react";

/** A delete that asks once more before it acts. `onDelete` reports whether
 * the engine deleted; a refusal (already alerted) disarms the question. */
export function ConfirmDelete(props: {
  label: string;
  question: string;
  onDelete: () => Promise<boolean>;
}) {
  const [armed, setArmed] = useState(false);
  const [busy, setBusy] = useState(false);
  return armed ? (
    <span className="inline-flex items-center gap-2 text-sm">
      <span>{props.question}</span>
      <button
        type="button"
        className="btn btn-xs btn-error"
        disabled={busy}
        onClick={async () => {
          setBusy(true);
          try {
            if (!(await props.onDelete())) setArmed(false);
          } finally {
            setBusy(false);
          }
        }}
      >
        Delete
      </button>
      <button type="button" className="btn btn-xs btn-ghost" onClick={() => setArmed(false)}>
        Keep
      </button>
    </span>
  ) : (
    <button
      type="button"
      className="btn btn-xs btn-outline btn-error"
      aria-label={props.label}
      onClick={() => setArmed(true)}
    >
      Delete…
    </button>
  );
}
