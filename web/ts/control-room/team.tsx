// Team: who may use the control room and as what role, and the audit log of
// every change they made. Admin-only; the engine refuses these routes to
// anyone else, so the views say so instead of rendering empty.

import { useCallback, useEffect, useMemo, useState } from "react";

import { createTeamMember, deleteTeamMember, getAudit, listTeam, updateTeamMember } from "../core/api.js";
import type { AuditEntry, Role, TeamMember } from "../core/api-types.js";
import { SearchField, ChipGroup } from "../ui/controls.js";
import type { Chip } from "../ui/controls.js";
import { DataTable } from "../ui/data-table.js";
import type { Column } from "../ui/data-table.js";
import { PageHeader } from "../ui/page-header.js";
import { EmptyState, Panel } from "../ui/panel.js";
import { StatusBadge } from "../ui/tone.js";
import type { Tone } from "../ui/tone.js";
import { ConfirmDelete } from "./confirm.js";
import { useSession } from "./session.js";
import { When } from "./when-view.js";
import {
  ROLES,
  ROLE_LABEL,
  ROLE_SUMMARY,
  auditOutcome,
  describeChange,
  filterAudit,
} from "./team-model.js";
import type { AuditOutcome } from "./team-model.js";

const MIN_PASSWORD = 12;
const BUILTIN_ADMIN = "admin";

function NotAdmin(props: { what: string }) {
  return (
    <Panel>
      <EmptyState
        title={`Only an admin sees ${props.what}.`}
        detail="The engine refuses these routes to other roles. Ask an admin, or switch back from View as."
      />
    </Panel>
  );
}

function RoleSelect(props: { value: Role; onChange: (role: Role) => void; disabled?: boolean; label: string }) {
  return (
    <select
      className="select select-sm w-full min-w-28"
      aria-label={props.label}
      value={props.value}
      disabled={props.disabled}
      onChange={(event) => props.onChange(event.target.value as Role)}
    >
      {ROLES.map((role) => (
        <option key={role} value={role}>
          {ROLE_LABEL[role]}
        </option>
      ))}
    </select>
  );
}

function AddMember(props: { onAdded: (member: TeamMember) => void }) {
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [role, setRole] = useState<Role>("viewer");
  const [busy, setBusy] = useState(false);
  const problem = !username.trim()
    ? "Name the member."
    : password.length < MIN_PASSWORD
      ? `A password needs at least ${MIN_PASSWORD} characters.`
      : null;
  return (
    <form
      className="grid gap-3 sm:grid-cols-2 xl:grid-cols-[1fr_1fr_10rem_auto] xl:items-end"
      onSubmit={async (event) => {
        event.preventDefault();
        if (problem) return;
        setBusy(true);
        try {
          const added = await createTeamMember({ username: username.trim(), password, role });
          if (added) {
            props.onAdded(added);
            setUsername("");
            setPassword("");
          }
        } finally {
          setBusy(false);
        }
      }}
    >
      <label className="flex min-w-0 flex-col gap-1 text-sm">
        <span className="kicker">Username</span>
        <input className="input input-sm w-full" autoComplete="off" value={username} onChange={(e) => setUsername(e.target.value)} />
      </label>
      <label className="flex min-w-0 flex-col gap-1 text-sm">
        <span className="kicker">Initial password</span>
        <input className="input input-sm w-full" type="password" autoComplete="new-password" value={password} onChange={(e) => setPassword(e.target.value)} />
      </label>
      <label className="flex min-w-0 flex-col gap-1 text-sm">
        <span className="kicker">Role</span>
        <RoleSelect value={role} onChange={setRole} label="New member's role" />
      </label>
      <button type="submit" className="btn btn-primary btn-sm" disabled={busy || problem !== null} title={problem ?? undefined}>
        Add member
      </button>
    </form>
  );
}

function ResetPassword(props: { member: TeamMember; onDone: () => void }) {
  const [open, setOpen] = useState(false);
  const [password, setPassword] = useState("");
  if (!open) {
    return (
      <button type="button" className="btn btn-ghost btn-xs" onClick={() => setOpen(true)}>
        Reset password
      </button>
    );
  }
  return (
    <span className="inline-flex flex-wrap items-center gap-2">
      <input
        className="input input-xs w-40"
        type="password"
        autoComplete="new-password"
        aria-label={`New password for ${props.member.username}`}
        value={password}
        onChange={(event) => setPassword(event.target.value)}
      />
      <button
        type="button"
        className="btn btn-primary btn-xs"
        disabled={password.length < MIN_PASSWORD}
        onClick={async () => {
          if (await updateTeamMember(props.member.id, { password })) {
            setOpen(false);
            setPassword("");
            props.onDone();
          }
        }}
      >
        Set
      </button>
      <button type="button" className="btn btn-ghost btn-xs" onClick={() => setOpen(false)}>
        Cancel
      </button>
    </span>
  );
}

export function Team() {
  const session = useSession();
  const [members, setMembers] = useState<TeamMember[] | null>(null);
  const reload = useCallback(() => {
    void listTeam().then(setMembers);
  }, []);
  useEffect(reload, [reload, session.role]);
  const isAdmin = session.can("admin");
  const counts = useMemo(() => {
    const byRole: Record<Role, number> = { viewer: 0, operator: 0, admin: 0 };
    for (const member of members ?? []) if (!member.disabled) byRole[member.role] += 1;
    return byRole;
  }, [members]);

  const replace = (updated: TeamMember | null) => {
    if (updated) setMembers((current) => current?.map((member) => (member.id === updated.id ? updated : member)) ?? null);
  };
  const columns: Column<TeamMember>[] = [
    {
      id: "member",
      header: "Member",
      width: "minmax(10rem,1.5fr)",
      sort: (member) => member.username,
      cell: (member) => (
        <span className="flex min-w-0 items-center gap-2">
          <Avatar name={member.username} dimmed={member.disabled} />
          <span className="truncate font-semibold">{member.username}</span>
          {member.username === session.principal?.username && <span className="text-xs text-base-content/60">(you)</span>}
        </span>
      ),
    },
    {
      id: "role",
      header: "Role",
      width: "10rem",
      sort: (member) => ROLES.indexOf(member.role),
      cell: (member) =>
        member.id === BUILTIN_ADMIN ? (
          <span title="The built-in admin is always an admin">{ROLE_LABEL.admin}</span>
        ) : (
          <RoleSelect
            label={`Role of ${member.username}`}
            value={member.role}
            onChange={async (role) => replace(await updateTeamMember(member.id, { role }))}
          />
        ),
    },
    {
      id: "status",
      header: "Status",
      width: "7rem",
      priority: 2,
      sort: (member) => (member.disabled ? 1 : 0),
      cell: (member) => <StatusBadge tone={member.disabled ? "neutral" : "ok"} dot>{member.disabled ? "Disabled" : "Active"}</StatusBadge>,
    },
    {
      id: "active",
      header: "Last active",
      width: "8rem",
      priority: 2,
      sort: (member) => member.lastActiveMs ?? 0,
      cell: (member) =>
        member.lastActiveMs ? <When atMs={member.lastActiveMs} show="ago" /> : <span className="text-base-content/60">never</span>,
    },
    {
      id: "since",
      header: "Since",
      width: "8rem",
      priority: 3,
      sort: (member) => member.createdAtMs,
      cell: (member) => (member.createdAtMs > 0 ? <When atMs={member.createdAtMs} show="ago" /> : "install"),
    },
    {
      id: "actions",
      header: <span className="sr-only">Actions</span>,
      width: "minmax(14rem,2fr)",
      bare: true,
      cell: (member) =>
        member.id === BUILTIN_ADMIN ? (
          <span className="text-xs text-base-content/60">Password under Settings → Security</span>
        ) : (
          <span className="flex flex-wrap items-center justify-end gap-1">
            <button
              type="button"
              className="btn btn-ghost btn-xs min-w-16"
              onClick={async () => replace(await updateTeamMember(member.id, { disabled: !member.disabled }))}
            >
              {member.disabled ? "Enable" : "Disable"}
            </button>
            <ResetPassword member={member} onDone={reload} />
            <ConfirmDelete
              label={`Remove ${member.username}`}
              question={`Remove ${member.username}?`}
              verb="Remove"
              onDelete={async () => {
                const removed = await deleteTeamMember(member.id);
                if (removed) setMembers((current) => current?.filter((candidate) => candidate.id !== member.id) ?? null);
                return removed;
              }}
            />
          </span>
        ),
    },
  ];

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        eyebrow="Manage · Team"
        title={members ? `${members.length} member${members.length === 1 ? "" : "s"}` : "Team"}
        subtitle="Each member signs in with their own password and acts with their role. Changes apply on their next request; disabling one ends their sessions."
      />
      {!isAdmin ? (
        <NotAdmin what="the team" />
      ) : (
        <>
          <section aria-label="Roles" className="grid gap-3 md:grid-cols-3">
            {ROLES.map((role) => (
              <div key={role} className="min-w-0 rounded-box border border-base-content/10 bg-base-200 p-3">
                <div className="flex items-baseline justify-between gap-2">
                  <span className="font-semibold">{ROLE_LABEL[role]}</span>
                  <span className="tabular text-sm text-base-content/70">{counts[role]} active</span>
                </div>
                <p className="mt-1 text-sm text-base-content/75">{ROLE_SUMMARY[role]}</p>
              </div>
            ))}
          </section>
          <Panel id="team-add" title="Add a member">
            <AddMember onAdded={(member) => setMembers((current) => [...(current ?? []), member])} />
          </Panel>
          <Panel id="team-members" title="Members" flush>
            <DataTable
              label="Team members"
              rows={members ?? []}
              columns={columns}
              rowKey={(member) => member.id}
              rowHeight={52}
              cardHeight={190}
              cardStatus={(member) => <StatusBadge tone={member.disabled ? "neutral" : "ok"}>{ROLE_LABEL[member.role]}</StatusBadge>}
              empty={<EmptyState title="Loading members…" />}
            />
          </Panel>
        </>
      )}
    </div>
  );
}

const OUTCOME_TONE: Record<Exclude<AuditOutcome, "all">, Tone> = { done: "ok", refused: "warn", failed: "bad" };
const OUTCOME_LABEL: Record<Exclude<AuditOutcome, "all">, string> = { done: "Done", refused: "Refused", failed: "Failed" };

export function Audit() {
  const session = useSession();
  const [entries, setEntries] = useState<AuditEntry[] | null>(null);
  const [exhausted, setExhausted] = useState(false);
  const [query, setQuery] = useState("");
  const [outcome, setOutcome] = useState<AuditOutcome>("all");
  const PAGE = 200;
  const load = useCallback(async () => {
    const page = await getAudit(undefined, PAGE);
    setEntries(page);
    setExhausted((page?.length ?? 0) < PAGE);
  }, []);
  useEffect(() => {
    void load();
  }, [load, session.role]);
  const older = async () => {
    const last = entries?.[entries.length - 1];
    if (!last) return;
    const page = await getAudit(last.id, PAGE);
    if (page) {
      setEntries((current) => [...(current ?? []), ...page]);
      setExhausted(page.length < PAGE);
    }
  };
  const shown = useMemo(() => filterAudit(entries ?? [], query, outcome), [entries, query, outcome]);
  const chips: Chip<AuditOutcome>[] = (["all", "done", "refused", "failed"] as const).map((value) => ({
    value,
    label: value === "all" ? "All" : OUTCOME_LABEL[value],
    count: value === "all" ? entries?.length : entries?.filter((entry) => auditOutcome(entry.status) === value).length,
    alert: value === "refused" || value === "failed",
  }));
  const columns: Column<AuditEntry>[] = [
    {
      id: "when",
      header: "When",
      width: "15rem",
      sort: (entry) => entry.atMs,
      cell: (entry) => <When atMs={entry.atMs} show="both" />,
    },
    {
      id: "who",
      header: "Who",
      width: "minmax(8rem,1fr)",
      sort: (entry) => entry.username,
      cell: (entry) => (
        <span className="truncate">
          {entry.username} <span className="text-base-content/60">as {ROLE_LABEL[entry.role].toLowerCase()}</span>
        </span>
      ),
    },
    {
      id: "change",
      header: "Change",
      width: "minmax(14rem,3fr)",
      cell: (entry) => (
        <span className="truncate" title={`${entry.method} ${entry.path}`}>
          {describeChange(entry)}
        </span>
      ),
    },
    {
      id: "outcome",
      header: "Outcome",
      width: "7rem",
      sort: (entry) => entry.status,
      cell: (entry) => {
        const result = auditOutcome(entry.status);
        return (
          <StatusBadge tone={OUTCOME_TONE[result]} title={`HTTP ${entry.status}`}>
            {OUTCOME_LABEL[result]}
          </StatusBadge>
        );
      },
    },
    {
      id: "request",
      header: "Request",
      width: "minmax(12rem,2fr)",
      priority: 3,
      cell: (entry) => (
        <code className="truncate text-xs text-base-content/70">
          {entry.method} {entry.path} · {entry.status}
        </code>
      ),
    },
  ];
  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        eyebrow="Manage · Audit log"
        title="Every change, who made it, and how it ended"
        subtitle="The engine records each change request, including the ones it refused for lack of a role. Reads are not recorded."
      />
      {!session.can("admin") ? (
        <NotAdmin what="the audit log" />
      ) : (
        <Panel
          flush
          title="Changes"
          actions={
            <button type="button" className="btn btn-ghost btn-xs" onClick={() => void load()}>
              Refresh
            </button>
          }
        >
          <div className="flex flex-col gap-2 border-b border-base-content/10 p-3 md:flex-row md:items-center">
            <SearchField value={query} onChange={setQuery} placeholder="Member, pipeline, destination…" label="Filter the audit log" className="md:w-80" />
            <ChipGroup label="Outcome" value={outcome} chips={chips} onChange={setOutcome} />
          </div>
          <DataTable
            label="Audit log"
            rows={shown}
            columns={columns}
            rowKey={(entry) => String(entry.id)}
            initialSort={{ id: "when", dir: "desc" }}
            cardTitle={(entry) => describeChange(entry)}
            cardStatus={(entry) => <StatusBadge tone={OUTCOME_TONE[auditOutcome(entry.status)]}>{OUTCOME_LABEL[auditOutcome(entry.status)]}</StatusBadge>}
            empty={<EmptyState title={entries === null ? "Loading…" : "No changes match."} />}
          />
          {!exhausted && entries && entries.length > 0 && (
            <div className="border-t border-base-content/10 p-3 text-center">
              <button type="button" className="btn btn-ghost btn-sm" onClick={() => void older()}>
                Load older
              </button>
            </div>
          )}
        </Panel>
      )}
    </div>
  );
}

const AVATAR_TONES = ["bg-primary/20 text-primary", "bg-secondary/20 text-secondary", "bg-accent/20 text-accent", "bg-info/20 text-info", "bg-success/20 text-success", "bg-warning/20 text-warning"];

/** Initials on a colour picked from the name, so a member is recognisable
 * at a glance in a long list: "ana.ops" is AO. */
function Avatar(props: { name: string; dimmed?: boolean }) {
  const parts = props.name.split(/[._\-\s]+/).filter(Boolean);
  const initials = ((parts[0]?.[0] ?? "?") + (parts[1]?.[0] ?? parts[0]?.[1] ?? "")).toUpperCase();
  let hash = 0;
  for (const char of props.name) hash = (hash * 31 + char.charCodeAt(0)) >>> 0;
  return (
    <span aria-hidden="true" className={`grid size-7 shrink-0 place-items-center rounded-full text-[0.7rem] font-bold ${AVATAR_TONES[hash % AVATAR_TONES.length]} ${props.dimmed ? "opacity-50" : ""}`}>
      {initials}
    </span>
  );
}
