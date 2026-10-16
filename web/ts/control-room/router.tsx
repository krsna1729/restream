// The control room's router: one route whose search is the place (route.ts),
// so reload, back/forward and shared links land on the same view. TanStack
// Router owns history and link clicks; `usePlace`, `PlaceLink` and `useGo`
// keep the app's own vocabulary for it. `createControlRoomRouter` takes the
// root component, so this module does not import the app that imports it.

import {
  Link,
  createRootRoute,
  createRoute,
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
  const rootRoute = createRootRoute({ component: root });
  const placeRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/",
    validateSearch: (search: Record<string, unknown>): PlaceSearch =>
      placeSearch(search, placeFromSearch(search)),
  });
  return createRouter({
    routeTree: rootRoute.addChildren([placeRoute]),
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
  return placeFromSearch(useSearch({ from: "/" }));
}

/** A link to a place: a real href, and the router handles the click. */
export function PlaceLink(props: {
  place: Place;
  current?: boolean;
  className?: string;
  children: ReactNode;
}) {
  return (
    <Link
      to="/"
      search={(previous: Record<string, unknown>) =>
        placeSearch(previous, props.place)
      }
      aria-current={props.current ? "page" : undefined}
      className={props.className}
    >
      {props.children}
    </Link>
  );
}

/** Moves to a place; the router updates the URL and history. */
export function useGo(): (place: Place) => void {
  const navigate = useNavigate();
  return (place) =>
    void navigate({
      to: "/",
      search: (previous) => placeSearch(previous, place),
    });
}
