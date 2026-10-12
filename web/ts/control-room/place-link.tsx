import type { ReactNode } from "react";

import { placeSearch } from "./route.js";
import type { Place } from "./route.js";

/** Moves to `place` in history; the app follows `popstate`. */
export function go(place: Place): void {
  const search = placeSearch(window.location.search, place);
  if (search === window.location.search) return;
  window.history.pushState({}, "", search);
  window.dispatchEvent(new PopStateEvent("popstate"));
}

/** A real link (open in a new tab works) that navigates in place. */
export function PlaceLink(props: {
  place: Place;
  className?: string;
  current?: boolean;
  children: ReactNode;
}) {
  return (
    <a
      href={placeSearch(window.location.search, props.place)}
      aria-current={props.current ? "page" : undefined}
      className={props.className}
      onClick={(event) => {
        if (event.metaKey || event.ctrlKey || event.shiftKey || event.button !== 0) {
          return;
        }
        event.preventDefault();
        go(props.place);
      }}
    >
      {props.children}
    </a>
  );
}
