// A retrying destination's next try, ticking: under a minute the seconds
// roll (DaisyUI countdown), longer waits read in words.

import type { CSSProperties } from "react";

import { useNow } from "./store.js";
import { retryWords } from "./when.js";

export function RetryCountdown(props: { atMs: number; attempts: number; className?: string }) {
  const now = useNow(1000);
  const left = Math.max(0, props.atMs - now);
  const seconds = Math.ceil(left / 1000);
  const words = retryWords(props.attempts, left);
  return (
    <span className={`tabular whitespace-nowrap ${props.className ?? "text-base-content/70"}`} title={words}>
      {left > 500 && seconds < 60 ? (
        <>
          Attempt {props.attempts} · next try in{" "}
          <span className="countdown">
            <span style={{ "--value": seconds } as CSSProperties} aria-live="polite" aria-label={`${seconds}`}>
              {seconds}
            </span>
          </span>{" "}
          s
        </>
      ) : (
        words
      )}
    </span>
  );
}
