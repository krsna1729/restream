// Small layout hooks shared by the primitives.

import { useEffect, useLayoutEffect, useRef, useState } from "react";

/** Whether a media query matches, updated as it changes. */
export function useMediaQuery(query: string): boolean {
  const get = () => typeof window.matchMedia === "function" && window.matchMedia(query).matches;
  const [matches, setMatches] = useState(get);
  useEffect(() => {
    if (typeof window.matchMedia !== "function") return;
    const list = window.matchMedia(query);
    const update = () => setMatches(list.matches);
    update();
    list.addEventListener("change", update);
    return () => list.removeEventListener("change", update);
  }, [query]);
  return matches;
}

/** Below Tailwind's `md` (768px): phone layout. */
export function usePhone(): boolean {
  return useMediaQuery("(max-width: 767.98px)");
}

/**
 * Windowing for a fixed-row-height list inside its own scroll container:
 * which rows to render, and the spacer heights around them.
 */
export function useWindowedRows(count: number, rowHeight: number, overscan = 8) {
  const ref = useRef<HTMLDivElement | null>(null);
  const [viewport, setViewport] = useState({ top: 0, height: 600 });
  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    const measure = () => setViewport({ top: element.scrollTop, height: element.clientHeight || 600 });
    measure();
    const onScroll = () => setViewport((current) => (current.top === element.scrollTop ? current : { top: element.scrollTop, height: element.clientHeight || current.height }));
    element.addEventListener("scroll", onScroll, { passive: true });
    const observer = typeof ResizeObserver === "function" ? new ResizeObserver(measure) : null;
    observer?.observe(element);
    return () => {
      element.removeEventListener("scroll", onScroll);
      observer?.disconnect();
    };
  }, [count > 0]);
  const first = Math.max(0, Math.floor(viewport.top / rowHeight) - overscan);
  const last = Math.min(count, Math.ceil((viewport.top + viewport.height) / rowHeight) + overscan);
  return {
    ref,
    first,
    last,
    before: first * rowHeight,
    after: Math.max(0, (count - last) * rowHeight),
  };
}
