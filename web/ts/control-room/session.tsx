// Who is signed in and the role the dashboard acts as. One query, shared by
// the shell (navigation, the user menu, the view-as banner) and any view
// that hides what the role would be refused. The server enforces the role;
// this only keeps the dashboard from offering what it would refuse.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback } from "react";

import { getSession, logout, setViewAs } from "../core/api.js";
import type { Role, SessionPrincipal } from "../core/api-types.js";
import { atLeast } from "./team-model.js";

const SESSION_KEY = ["session"];

export interface Session {
  /** Null while loading, or for an engine without team support. */
  readonly principal: SessionPrincipal | null;
  /** The role the dashboard acts as: the view, or the member's own. An
   * unknown session is treated as an admin, as before roles existed. */
  readonly role: Role;
  readonly can: (needed: Role) => boolean;
  readonly viewAs: (role: Role | null) => Promise<void>;
  readonly signOut: () => Promise<void>;
}

export function useSession(): Session {
  const client = useQueryClient();
  const query = useQuery({
    queryKey: SESSION_KEY,
    queryFn: async () => (await getSession()) ?? null,
    staleTime: 60_000,
    refetchInterval: 60_000,
  });
  const principal = query.data ?? null;
  const role = principal?.effectiveRole ?? "admin";
  const viewAs = useCallback(
    async (next: Role | null) => {
      const updated = await setViewAs(next);
      if (updated) {
        client.setQueryData(SESSION_KEY, updated);
        // Every view reloads as the new role sees it.
        await client.invalidateQueries({ predicate: (candidate) => candidate.queryKey[0] !== "session" });
      }
    },
    [client],
  );
  const signOut = useCallback(async () => {
    await logout();
    window.location.reload();
  }, []);
  return { principal, role, can: (needed: Role) => atLeast(role, needed), viewAs, signOut };
}
