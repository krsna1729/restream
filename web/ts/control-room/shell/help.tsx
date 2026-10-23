// On-screen help: a drawer that says what the current page is for, and a
// spotlight tour that walks a page (or, for onboarding, the whole console)
// one highlighted element at a time. The words live in help-model.ts.

import { createContext, useCallback, useContext, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties, ReactNode } from "react";
import { createPortal } from "react-dom";

import { Icon } from "../../ui/icons.js";
import { ONBOARDING, PAGE_HELP, moveStep, stepsFor } from "../help-model.js";
import type { TourStep } from "../help-model.js";
import { PIPELINE_WORKSPACES } from "./nav.js";
import { WORKSPACE_LABELS } from "../route.js";
import { PlaceLink, useGo, usePlace } from "../router.js";
import { useSession } from "../session.js";

interface TourApi {
  /** Starts a tour; an empty one does nothing. */
  readonly start: (steps: readonly TourStep[]) => void;
  readonly startOnboarding: () => void;
  readonly startPage: () => void;
}

const TourContext = createContext<TourApi | null>(null);

export function useTour(): TourApi {
  const api = useContext(TourContext);
  if (!api) throw new Error("useTour outside TourProvider");
  return api;
}

/** Owns the running tour; moves between workspaces as its steps ask. */
export function TourProvider(props: { children: ReactNode; firstPipelineId: string | null }) {
  const [steps, setSteps] = useState<readonly TourStep[] | null>(null);
  const [index, setIndex] = useState(0);
  const place = usePlace();
  const go = useGo();
  const { role } = useSession();

  const start = useCallback(
    (next: readonly TourStep[]) => {
      const shown = stepsFor(next, role);
      if (shown.length === 0) return;
      setSteps(shown);
      setIndex(0);
    },
    [role],
  );
  const api = useMemo<TourApi>(
    () => ({
      start,
      startOnboarding: () => start(ONBOARDING),
      startPage: () => start(PAGE_HELP[place.workspace].tour),
    }),
    [start, place.workspace],
  );

  const step = steps?.[index] ?? null;
  const wanted = step?.workspace ?? null;
  useEffect(() => {
    if (wanted === null || wanted === place.workspace) return;
    // A pipeline page needs a pipeline: keep the one in view, else the first.
    const pipelineId = PIPELINE_WORKSPACES.has(wanted) ? (place.pipelineId ?? props.firstPipelineId) : place.pipelineId;
    go({ workspace: wanted, pipelineId });
  }, [wanted, place.workspace, place.pipelineId, props.firstPipelineId, go]);

  const close = useCallback(() => setSteps(null), []);
  const move = useCallback(
    (delta: 1 | -1) => {
      if (!steps) return;
      const next = moveStep(index, delta, steps.length);
      if (next === null) setSteps(null);
      else setIndex(next);
    },
    [steps, index],
  );

  return (
    <TourContext.Provider value={api}>
      {props.children}
      {steps && step && (wanted === null || wanted === place.workspace) && (
        <Spotlight key={`${index}-${step.title}`} step={step} index={index} count={steps.length} onMove={move} onClose={close} />
      )}
    </TourContext.Provider>
  );
}

/** The first visible element a selector matches. */
function findTarget(selector: string | undefined): HTMLElement | null {
  if (!selector) return null;
  for (const element of document.querySelectorAll<HTMLElement>(selector)) {
    const rect = element.getBoundingClientRect();
    if (rect.width > 0 && rect.height > 0) return element;
  }
  return null;
}

interface Box {
  readonly top: number;
  readonly left: number;
  readonly width: number;
  readonly height: number;
}

const PAD = 6;
const CARD_W = 340;

function Spotlight(props: { step: TourStep; index: number; count: number; onMove: (delta: 1 | -1) => void; onClose: () => void }) {
  const [box, setBox] = useState<Box | null>(null);
  const [searched, setSearched] = useState(false);
  const next = useRef<HTMLButtonElement | null>(null);
  const target = useRef<HTMLElement | null>(null);

  // The page may still be rendering after a move: look for up to 2 s.
  useEffect(() => {
    let cancelled = false;
    let tries = 0;
    const look = () => {
      if (cancelled) return;
      const found = findTarget(props.step.target);
      if (found || tries >= 20 || !props.step.target) {
        target.current = found;
        if (found) found.scrollIntoView({ block: "center", behavior: "instant" as ScrollBehavior });
        setSearched(true);
        return;
      }
      tries += 1;
      window.setTimeout(look, 100);
    };
    look();
    return () => {
      cancelled = true;
    };
  }, [props.step.target]);

  useLayoutEffect(() => {
    if (!searched) return;
    const measure = () => {
      const rect = target.current?.getBoundingClientRect();
      setBox(rect ? { top: rect.top - PAD, left: rect.left - PAD, width: rect.width + PAD * 2, height: rect.height + PAD * 2 } : null);
    };
    measure();
    window.addEventListener("resize", measure);
    window.addEventListener("scroll", measure, true);
    return () => {
      window.removeEventListener("resize", measure);
      window.removeEventListener("scroll", measure, true);
    };
  }, [searched]);

  useEffect(() => {
    if (searched) next.current?.focus();
  }, [searched]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") props.onClose();
      else if (event.key === "ArrowRight") props.onMove(1);
      else if (event.key === "ArrowLeft") props.onMove(-1);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [props]);

  if (!searched) return null;
  const last = props.index === props.count - 1;
  const narrow = window.innerWidth < 640;
  const card = cardPlace(box, narrow);
  return (
    <div className="fixed inset-0 z-[60]" data-tour-overlay>
      {/* A click outside the card does nothing: the tour holds the page. */}
      <div className="absolute inset-0" onClick={(event) => event.stopPropagation()} />
      {box ? (
        <div
          aria-hidden="true"
          className="pointer-events-none absolute rounded-box ring-2 ring-primary transition-all duration-200"
          style={{ ...box, boxShadow: "0 0 0 9999px color-mix(in oklab, black 55%, transparent)" }}
        />
      ) : (
        <div aria-hidden="true" className="absolute inset-0 bg-black/55" />
      )}
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="tour-title"
        aria-describedby="tour-body"
        className="absolute flex flex-col gap-2 rounded-box border border-base-content/15 bg-base-100 p-4 shadow-xl"
        style={card}
      >
        <div className="flex items-center gap-2">
          <span className="kicker">
            {props.index + 1} of {props.count}
          </span>
          <button type="button" className="btn btn-ghost btn-xs btn-square ml-auto" aria-label="End the tour" onClick={props.onClose}>
            <Icon name="close" className="size-4" />
          </button>
        </div>
        <h2 id="tour-title" className="font-semibold">
          {props.step.title}
        </h2>
        <p id="tour-body" className="text-sm text-base-content/80">
          {props.step.body}
        </p>
        <div className="mt-1 flex items-center gap-2">
          <progress className="progress progress-primary h-1 flex-1" value={props.index + 1} max={props.count} aria-hidden="true" />
          {props.index > 0 && (
            <button type="button" className="btn btn-sm btn-outline" onClick={() => props.onMove(-1)}>
              Back
            </button>
          )}
          <button ref={next} type="button" className="btn btn-sm btn-primary" onClick={() => props.onMove(1)}>
            {last ? "Done" : "Next"}
          </button>
        </div>
      </div>
    </div>
  );
}

/** Below the target if it fits, else above, else centred; docked on phones. */
function cardPlace(box: Box | null, narrow: boolean): CSSProperties {
  const vw = window.innerWidth;
  const vh = window.innerHeight;
  if (narrow) return { left: 12, right: 12, bottom: 84 };
  if (!box) return { left: (vw - CARD_W) / 2, top: vh / 2 - 90, width: CARD_W };
  const left = Math.min(Math.max(12, box.left), vw - CARD_W - 12);
  if (box.top + box.height + 200 < vh) return { left, top: box.top + box.height + 10, width: CARD_W };
  if (box.top > 210) return { left, top: box.top - 10, width: CARD_W, transform: "translateY(-100%)" };
  return { left: (vw - CARD_W) / 2, top: vh / 2 - 90, width: CARD_W };
}

/** The top bar's help button, and the `?` shortcut. */
export function HelpButton() {
  const [open, setOpen] = useState(false);
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const typing = event.target instanceof HTMLElement && event.target.closest("input, textarea, select, [contenteditable]");
      if (event.key === "?" && !typing && !event.metaKey && !event.ctrlKey) {
        event.preventDefault();
        setOpen((value) => !value);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
  return (
    <>
      <button
        type="button"
        className="btn btn-ghost btn-sm btn-square"
        aria-label="Help for this page"
        aria-expanded={open}
        title="Help (?)"
        onClick={() => setOpen(true)}
      >
        <Icon name="help" className="size-5" />
      </button>
      {open && <HelpDrawer onClose={() => setOpen(false)} />}
    </>
  );
}

function HelpDrawer(props: { onClose: () => void }) {
  const place = usePlace();
  const tour = useTour();
  const help = PAGE_HELP[place.workspace];
  const close = useRef<HTMLButtonElement | null>(null);
  useEffect(() => {
    close.current?.focus();
    const onKey = (event: KeyboardEvent) => event.key === "Escape" && props.onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [props]);
  const run = (begin: () => void) => {
    props.onClose();
    // Let the drawer leave before the spotlight measures the page.
    window.setTimeout(begin, 50);
  };
  // Portalled: the top bar's backdrop blur would otherwise clip a fixed child.
  return createPortal(
    <div className="fixed inset-0 z-50">
      <div className="absolute inset-0 bg-black/40" onClick={props.onClose} aria-hidden="true" />
      <aside
        role="dialog"
        aria-modal="true"
        aria-labelledby="help-title"
        className="absolute inset-y-0 right-0 flex w-full max-w-sm flex-col gap-4 overflow-y-auto border-l border-base-content/10 bg-base-100 p-5 shadow-xl"
      >
        <div className="flex items-start gap-2">
          <div className="min-w-0">
            <div className="kicker text-primary">Help · {WORKSPACE_LABELS[place.workspace]}</div>
            <h2 id="help-title" className="mt-1 text-lg leading-snug font-semibold text-balance">
              {help.purpose}
            </h2>
          </div>
          <button ref={close} type="button" className="btn btn-ghost btn-sm btn-square ml-auto shrink-0" aria-label="Close help" onClick={props.onClose}>
            <Icon name="close" className="size-4" />
          </button>
        </div>
        <p className="text-sm text-base-content/80">{help.when}</p>
        {help.terms.length > 0 && (
          <dl className="flex flex-col gap-2 rounded-box bg-base-200 p-3 text-sm">
            {help.terms.map(([term, meaning]) => (
              <div key={term}>
                <dt className="font-semibold">{term}</dt>
                <dd className="text-base-content/80">{meaning}</dd>
              </div>
            ))}
          </dl>
        )}
        <div className="flex flex-col gap-2">
          <button type="button" className="btn btn-primary btn-sm" onClick={() => run(tour.startPage)}>
            Show me around this page
          </button>
          <button type="button" className="btn btn-outline btn-sm" onClick={() => run(tour.startOnboarding)}>
            Take the full tour
          </button>
        </div>
        <div className="mt-auto flex flex-col gap-1 border-t border-base-content/10 pt-3 text-sm">
          <PlaceLink place={{ workspace: "guides", pipelineId: place.pipelineId }} className="link link-hover" >
            <span onClick={props.onClose}>Guides: step-by-step tasks</span>
          </PlaceLink>
          <PlaceLink place={{ workspace: "architecture", pipelineId: place.pipelineId }} className="link link-hover">
            <span onClick={props.onClose}>How it works: the system as a whole</span>
          </PlaceLink>
          <p className="text-xs text-base-content/70">
            Press <kbd className="kbd kbd-xs">?</kbd> to open this from anywhere.
          </p>
        </div>
      </aside>
    </div>,
    document.body,
  );
}
