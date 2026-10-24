// The control room's router: one route, the root, whose search is the place
// (route.ts), so reload, back/forward and shared links land on the same view
// on any path the engine serves the page from. TanStack Router owns history
// and link clicks; `usePlace`, `PlaceLink` and `useGo` keep the app's own
// vocabulary for it. `createControlRoomRouter` takes the root component, so
// this module does not import the app that imports it.

import {
  Link,
  createRootRoute,
  createRouter,
  useNavigate,
  useSearch,
} from "@tanstack/react-router";
import type { ReactNode } from "react";

import { getBasePath } from "../core/base-path.js";
import {
  parseSearch,
  placeFromSearch,
  placeSearch,
  stringifySearch,
} from "./route.js";
import type { Place, PlaceSearch } from "./route.js";

/** The control room's router over `root`, which renders the app. */
export function createControlRoomRouter(root: () => ReactNode) {
  // The search lives on the root route, which matches every path: the engine
  // serves the page for unknown paths, so a stale deep link
  // (`/index.html?w=operate`) must still render the workspace it names.
  const rootRoute = createRootRoute({
    component: root,
    validateSearch: (search: Record<string, unknown>): PlaceSearch =>
      placeSearch(search, placeFromSearch(search)),
  });
  return createRouter({
    routeTree: rootRoute,
    basepath: getBasePath() || "/",
    parseSearch,
    stringifySearch,
  });
}

declare module "@tanstack/react-router" {
  interface Register {
    router: ReturnType<typeof createControlRoomRouter>;
  }
}

/** The place the URL names. */
export function usePlace(): Place {
  return placeFromSearch(useSearch({ from: "__root__" }));
}

/** A link to a place: a real href, and the router handles the click. */
export function PlaceLink(props: {
  place: Place;
  current?: boolean;
  className?: string;
  onClick?: () => void;
  children: ReactNode;
}) {
  const here = usePlace();
  return (
    <Link
      viewTransition={here.workspace !== props.place.workspace}
      to="/"
      search={(previous: Record<string, unknown>) =>
        placeSearch(previous, props.place)
      }
      aria-current={props.current ? "page" : undefined}
      className={props.className}
      onClick={props.onClick}
    >
      {props.children}
    </Link>
  );
}

/** Moves to a place; the router updates the URL and history. A move to
 * another workspace crossfades where the browser has View Transitions
 * (input.css drops it under reduced motion); a filter or a pipeline
 * change within one swaps at once, so what was asked for shows now. */
export function useGo(): (place: Place) => void {
  const navigate = useNavigate();
  const here = usePlace();
  return (place) =>
    void navigate({
      to: "/",
      search: (previous) => placeSearch(previous, place),
      viewTransition: here.workspace !== place.workspace,
    });
}
