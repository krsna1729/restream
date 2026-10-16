// The control room's router: one route whose search is the place (route.ts),
// so reload, back/forward and shared links land on the same view. TanStack
// Router owns history and link clicks; `usePlace`, `PlaceLink` and `useGo`
// keep the app's own vocabulary for it.

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
import { App } from "./app.js";
import { placeFromSearch, placeSearch } from "./route.js";
import type { Place, PlaceSearch } from "./route.js";

const rootRoute = createRootRoute({ component: App });

/** The one route: the place in the search. */
const placeRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
  validateSearch: (search: Record<string, unknown>): PlaceSearch =>
    placeSearch(search, placeFromSearch(search)),
});

export const router = createRouter({
  routeTree: rootRoute.addChildren([placeRoute]),
  basepath: getBasePath() || "/",
});

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
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
