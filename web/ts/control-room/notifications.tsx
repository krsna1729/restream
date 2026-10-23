// Notifications: where the engine sends alerts. Every member sets up this
// browser (Web Push with no tab open, or in-tab notifications); an admin
// manages the shared channels (Slack, Discord, signed webhooks) and reads
// how each delivery went.

import { useCallback, useEffect, useMemo, useState } from "react";

import {
  createNotifyChannel,
  deleteNotifyChannel,
  getNotifyChannelSecret,
  listNotifyChannels,
  listNotifyDeliveries,
  testNotifyChannel,
  testPush,
  updateNotifyChannel,
} from "../core/api.js";
import type {
  ChannelKind,
  NotifyChannel,
  NotifyChannelDraft,
  NotifyDelivery,
  NotifySeverity,
  PushSubscriptionRecord,
} from "../core/api-types.js";
import { DataTable } from "../ui/data-table.js";
import type { Column } from "../ui/data-table.js";
import { PageHeader } from "../ui/page-header.js";
import { EmptyState, Panel } from "../ui/panel.js";
import { StatusBadge } from "../ui/tone.js";
import {
  currentPush,
  disablePush,
  enablePush,
  inTabSetting,
  notificationPermission,
  pushSupport,
  setInTabSetting,
} from "./browser-notify.js";
import { ConfirmDelete } from "./confirm.js";
import { When } from "./when-view.js";
import { CHANNEL_KINDS, channelHealth, channelProblem, guessKind } from "./notify-model.js";
import { useSession } from "./session.js";

const SEVERITY_LABEL: Record<NotifySeverity, string> = {
  critical: "Critical only",
  warning: "Warnings and critical",
};

function SeveritySelect(props: { value: NotifySeverity; onChange: (value: NotifySeverity) => void; label: string }) {
  return (
    <select className="select select-sm w-full" aria-label={props.label} value={props.value} onChange={(e) => props.onChange(e.target.value as NotifySeverity)}>
      {(["critical", "warning"] as const).map((value) => (
        <option key={value} value={value}>
          {SEVERITY_LABEL[value]}
        </option>
      ))}
    </select>
  );
}

function ThisBrowser() {
  const support = pushSupport();
  const [record, setRecord] = useState<PushSubscriptionRecord | null>(null);
  const [severity, setSeverity] = useState<NotifySeverity>("critical");
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [inTab, setInTab] = useState(inTabSetting());
  const [permission, setPermission] = useState(notificationPermission());
  useEffect(() => {
    void currentPush().then(setRecord);
  }, []);

  return (
    <Panel id="notify-browser" title="This browser" subtitle="Alerts as system notifications on this device">
      <div className="grid gap-4 lg:grid-cols-2">
        <div className="flex min-w-0 flex-col gap-2">
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-semibold">Push</span>
            {record ? (
              <StatusBadge tone="ok" dot>On · {SEVERITY_LABEL[record.minSeverity].toLowerCase()}</StatusBadge>
            ) : (
              <StatusBadge tone="neutral">Off</StatusBadge>
            )}
          </div>
          <p className="text-sm text-base-content/75">
            The engine sends alerts to this browser even with no restream tab open. It needs HTTPS (or localhost) and a
            push service the browser can reach.
          </p>
          {support === "supported" ? (
            <div className="flex flex-wrap items-end gap-2">
              {!record && (
                <label className="flex w-56 flex-col gap-1 text-sm">
                  <span className="kicker">Send me</span>
                  <SeveritySelect value={severity} onChange={setSeverity} label="Push severity" />
                </label>
              )}
              {record ? (
                <>
                  <button
                    type="button"
                    className="btn btn-sm"
                    disabled={busy}
                    onClick={async () => {
                      setBusy(true);
                      const deliveries = await testPush();
                      setBusy(false);
                      setMessage(
                        deliveries?.length
                          ? deliveries.every((delivery) => delivery.ok)
                            ? "Test sent: a notification should appear in a moment."
                            : `The push service refused it (${deliveries.find((delivery) => !delivery.ok)?.detail}).`
                          : "No subscription to send to.",
                      );
                    }}
                  >
                    Send a test
                  </button>
                  <button
                    type="button"
                    className="btn btn-ghost btn-sm border-base-content/15"
                    onClick={async () => {
                      await disablePush(record);
                      setRecord(null);
                      setMessage("Push is off for this browser.");
                    }}
                  >
                    Turn off
                  </button>
                </>
              ) : (
                <button
                  type="button"
                  className="btn btn-primary btn-sm"
                  disabled={busy}
                  onClick={async () => {
                    setBusy(true);
                    const result = await enablePush(severity);
                    setBusy(false);
                    setPermission(notificationPermission());
                    if (typeof result === "string") setMessage(result);
                    else {
                      setRecord(result);
                      setMessage("Push is on for this browser.");
                    }
                  }}
                >
                  Turn on push
                </button>
              )}
            </div>
          ) : (
            <p className="text-sm text-warning">
              {support === "insecure" ? "Push needs this dashboard on HTTPS (or localhost)." : "This browser cannot receive push notifications."}
            </p>
          )}
          {message && (
            <p role="status" className="text-sm">
              {message}
            </p>
          )}
        </div>
        <div className="flex min-w-0 flex-col gap-2">
          <span className="font-semibold">While a tab is open</span>
          <p className="text-sm text-base-content/75">
            A notification when an alert opens and this tab is in the background. Works without push.
          </p>
          <label className="flex w-56 flex-col gap-1 text-sm">
            <span className="kicker">In-tab notifications</span>
            <select
              className="select select-sm w-full"
              aria-label="In-tab notifications"
              value={inTab}
              onChange={async (event) => {
                const value = event.target.value as NotifySeverity | "off";
                if (value !== "off" && notificationPermission() === "default") {
                  setPermission(await Notification.requestPermission());
                }
                setInTabSetting(value);
                setInTab(value);
              }}
            >
              <option value="off">Off</option>
              <option value="critical">Critical only</option>
              <option value="warning">Warnings and critical</option>
            </select>
          </label>
          {permission === "denied" && inTab !== "off" && (
            <p className="text-sm text-warning">This site's notifications are blocked in the browser's settings.</p>
          )}
        </div>
      </div>
    </Panel>
  );
}

const EMPTY_DRAFT: NotifyChannelDraft = {
  kind: "slack",
  name: "",
  target: "",
  minSeverity: "critical",
  onResolved: true,
  enabled: true,
};

function ChannelForm(props: { initial: NotifyChannelDraft; editing: boolean; onSave: (draft: NotifyChannelDraft) => Promise<void>; onCancel?: () => void }) {
  const [draft, setDraft] = useState(props.initial);
  const [busy, setBusy] = useState(false);
  const problem = channelProblem(draft);
  const set = (change: Partial<NotifyChannelDraft>) => setDraft((current) => ({ ...current, ...change }));
  return (
    <form
      className="flex flex-col gap-3"
      onSubmit={async (event) => {
        event.preventDefault();
        if (problem) return;
        setBusy(true);
        try {
          await props.onSave(draft);
          if (!props.editing) setDraft(EMPTY_DRAFT);
        } finally {
          setBusy(false);
        }
      }}
    >
      <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-[9rem_1fr_2fr]">
        <label className="flex min-w-0 flex-col gap-1 text-sm">
          <span className="kicker">Kind</span>
          <select className="select select-sm w-full" aria-label="Channel kind" value={draft.kind} disabled={props.editing} onChange={(e) => set({ kind: e.target.value as ChannelKind })}>
            {(Object.keys(CHANNEL_KINDS) as ChannelKind[]).map((kind) => (
              <option key={kind} value={kind}>
                {CHANNEL_KINDS[kind].label}
              </option>
            ))}
          </select>
        </label>
        <label className="flex min-w-0 flex-col gap-1 text-sm">
          <span className="kicker">Name</span>
          <input className="input input-sm w-full" aria-label="Channel name" placeholder="#ops-alerts" value={draft.name} onChange={(e) => set({ name: e.target.value })} />
        </label>
        <label className="flex min-w-0 flex-col gap-1 text-sm sm:col-span-2 xl:col-span-1">
          <span className="kicker">URL</span>
          <input
            className="input input-sm w-full"
            aria-label="Channel URL"
            placeholder={CHANNEL_KINDS[draft.kind].placeholder}
            value={draft.target}
            onChange={(e) => {
              const target = e.target.value;
              const kind = props.editing ? null : guessKind(target);
              set(kind ? { target, kind } : { target });
            }}
          />
        </label>
      </div>
      <p className="text-xs text-base-content/70">{CHANNEL_KINDS[draft.kind].hint}</p>
      <div className="flex flex-wrap items-end gap-3">
        <label className="flex w-56 flex-col gap-1 text-sm">
          <span className="kicker">Send</span>
          <SeveritySelect value={draft.minSeverity} onChange={(minSeverity) => set({ minSeverity })} label="Channel severity" />
        </label>
        <label className="flex h-8 items-center gap-2 text-sm">
          <input type="checkbox" className="checkbox checkbox-sm" checked={draft.onResolved} onChange={(e) => set({ onResolved: e.target.checked })} />
          Also when resolved
        </label>
        <label className="flex h-8 items-center gap-2 text-sm">
          <input type="checkbox" className="checkbox checkbox-sm" checked={draft.enabled} onChange={(e) => set({ enabled: e.target.checked })} />
          Enabled
        </label>
        <div className="ml-auto flex gap-2">
          {props.onCancel && (
            <button type="button" className="btn btn-ghost btn-sm" onClick={props.onCancel}>
              Cancel
            </button>
          )}
          <button type="submit" className="btn btn-primary btn-sm" disabled={busy || problem !== null} title={problem ?? undefined}>
            {props.editing ? "Save channel" : "Add channel"}
          </button>
        </div>
      </div>
    </form>
  );
}

function Channels() {
  const [channels, setChannels] = useState<NotifyChannel[] | null>(null);
  const [deliveries, setDeliveries] = useState<NotifyDelivery[]>([]);
  const [editing, setEditing] = useState<string | null>(null);
  const [secret, setSecret] = useState<{ name: string; value: string } | null>(null);
  const [tested, setTested] = useState<Record<string, NotifyDelivery>>({});
  const reload = useCallback(() => {
    void listNotifyChannels().then(setChannels);
    void listNotifyDeliveries(200).then((list) => setDeliveries(list ?? []));
  }, []);
  useEffect(reload, [reload]);
  useEffect(() => {
    const timer = window.setInterval(() => void listNotifyDeliveries(200).then((list) => setDeliveries(list ?? [])), 10_000);
    return () => window.clearInterval(timer);
  }, []);
  const health = useMemo(() => channelHealth(deliveries), [deliveries]);

  const columns: Column<NotifyChannel>[] = [
    {
      id: "channel",
      header: "Channel",
      width: "minmax(12rem,2fr)",
      sort: (channel) => channel.name,
      cell: (channel) => (
        <span className="flex min-w-0 flex-col">
          <span className="truncate font-semibold">{channel.name}</span>
          <span className="truncate text-xs text-base-content/65">{CHANNEL_KINDS[channel.kind].label} · {channel.target.replace(/^https?:\/\//, "").slice(0, 48)}</span>
        </span>
      ),
    },
    {
      id: "sends",
      header: "Sends",
      width: "minmax(10rem,1.2fr)",
      priority: 3,
      cell: (channel) => (
        <span className="truncate text-sm">
          {SEVERITY_LABEL[channel.minSeverity]}
          {channel.onResolved ? " · resolved" : ""}
        </span>
      ),
    },
    {
      id: "last",
      header: "Last delivery",
      width: "minmax(10rem,1.3fr)",
      cell: (channel) => {
        const last = tested[channel.id] ?? health.get(channel.id)?.last ?? null;
        if (!channel.enabled) return <StatusBadge tone="neutral">Disabled</StatusBadge>;
        if (!last) return <span className="text-sm text-base-content/60">None yet</span>;
        return (
          <span className="flex min-w-0 items-center gap-2">
            <StatusBadge tone={last.ok ? "ok" : "bad"} dot title={last.detail}>
              {last.ok ? "Delivered" : "Failed"}
            </StatusBadge>
            <span className="truncate text-xs text-base-content/65">
              <When atMs={last.atMs} show="ago" /> · {last.detail}
            </span>
          </span>
        );
      },
    },
    {
      id: "actions",
      header: <span className="sr-only">Actions</span>,
      width: "minmax(12rem,1.6fr)",
      bare: true,
      cell: (channel) => (
        <span className="flex flex-wrap items-center gap-1">
          <button
            type="button"
            className="btn btn-ghost btn-xs"
            onClick={async () => {
              const delivery = await testNotifyChannel(channel.id);
              if (delivery) setTested((current) => ({ ...current, [channel.id]: delivery }));
              reload();
            }}
          >
            Send test
          </button>
          <button type="button" className="btn btn-ghost btn-xs" onClick={() => setEditing(channel.id)}>
            Edit
          </button>
          {channel.kind === "webhook" && (
            <button
              type="button"
              className="btn btn-ghost btn-xs"
              onClick={async () => {
                const value = await getNotifyChannelSecret(channel.id);
                if (value) setSecret({ name: channel.name, value });
              }}
            >
              Secret
            </button>
          )}
          <ConfirmDelete
            label={`Delete ${channel.name}`}
            question="Delete this channel?"
            onDelete={async () => {
              const deleted = await deleteNotifyChannel(channel.id);
              if (deleted) reload();
              return deleted;
            }}
          />
        </span>
      ),
    },
  ];
  const editedChannel = channels?.find((channel) => channel.id === editing) ?? null;

  const deliveryColumns: Column<NotifyDelivery>[] = [
    {
      id: "when",
      header: "When",
      width: "13rem",
      sort: (delivery) => delivery.atMs,
      cell: (delivery) => <When atMs={delivery.atMs} show="both" seconds />,
    },
    { id: "to", header: "To", width: "minmax(9rem,1fr)", sort: (delivery) => delivery.targetName, cell: (delivery) => <span className="truncate">{delivery.targetName}</span> },
    {
      id: "what",
      header: "What",
      width: "minmax(12rem,2fr)",
      cell: (delivery) => (
        <span className="truncate">
          <span className="text-base-content/60">{delivery.event}</span> · {delivery.title}
        </span>
      ),
    },
    {
      id: "result",
      header: "Result",
      width: "minmax(9rem,1fr)",
      sort: (delivery) => (delivery.ok ? 1 : 0),
      cell: (delivery) => (
        <span className="flex min-w-0 items-center gap-2">
          <StatusBadge tone={delivery.ok ? "ok" : "bad"}>{delivery.ok ? "Delivered" : "Failed"}</StatusBadge>
          <span className="truncate text-xs text-base-content/65">
            {delivery.detail}
            {delivery.attempts > 1 ? ` · ${delivery.attempts} tries` : ""}
          </span>
        </span>
      ),
    },
  ];

  return (
    <>
      <Panel id="notify-add" title={editedChannel ? `Edit ${editedChannel.name}` : "Add a channel"}>
        {editedChannel ? (
          <ChannelForm
            key={editedChannel.id}
            editing
            initial={editedChannel}
            onCancel={() => setEditing(null)}
            onSave={async (draft) => {
              if (await updateNotifyChannel(editedChannel.id, draft)) {
                setEditing(null);
                reload();
              }
            }}
          />
        ) : (
          <ChannelForm
            editing={false}
            initial={EMPTY_DRAFT}
            onSave={async (draft) => {
              const created = await createNotifyChannel(draft);
              if (created) {
                if (created.secret) setSecret({ name: created.channel.name, value: created.secret });
                reload();
              }
            }}
          />
        )}
        {secret && (
          <div role="status" className="mt-3 flex flex-wrap items-center gap-2 rounded-box border border-info/30 bg-info/10 p-3 text-sm">
            <span>
              Signing secret for <strong>{secret.name}</strong>: verify <code>X-Restream-Signature</code> (HMAC-SHA256 of
              <code> t.body</code>) with
            </span>
            <code className="break-all rounded bg-base-100 px-2 py-1">{secret.value}</code>
            <button type="button" className="btn btn-ghost btn-xs ml-auto" onClick={() => setSecret(null)}>
              Hide
            </button>
          </div>
        )}
      </Panel>
      <Panel id="notify-channels" title="Channels" subtitle="Shared by the whole team" flush>
        <DataTable
          label="Notification channels"
          rows={channels ?? []}
          columns={columns}
          rowKey={(channel) => channel.id}
          rowHeight={56}
          cardHeight={200}
          empty={<EmptyState title={channels === null ? "Loading…" : "No channels yet."} detail="Add Slack, Discord or a webhook above; each alert that opens (and, if asked, resolves) is sent to every channel that wants its severity." />}
        />
      </Panel>
      <Panel id="notify-deliveries" title="Deliveries" subtitle="Newest first · three tries over about 20 s, then given up" flush>
        <DataTable
          label="Notification deliveries"
          rows={deliveries}
          columns={deliveryColumns}
          rowKey={(delivery) => String(delivery.id)}
          initialSort={{ id: "when", dir: "desc" }}
          cardTitle={(delivery) => delivery.title}
          cardStatus={(delivery) => <StatusBadge tone={delivery.ok ? "ok" : "bad"}>{delivery.ok ? "Delivered" : "Failed"}</StatusBadge>}
          empty={<EmptyState title="Nothing sent yet." />}
        />
      </Panel>
    </>
  );
}

export function Notifications() {
  const session = useSession();
  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        eyebrow="Manage · Notifications"
        title="Who hears about alerts, and how"
        subtitle="The engine checks alerts every 5 s, with or without a dashboard open. An alert must hold for two checks before anyone is told, so a flicker pages nobody."
      />
      <ThisBrowser />
      {session.can("admin") ? (
        <Channels />
      ) : (
        <Panel>
          <EmptyState title="An admin manages the shared channels." detail="Slack, Discord and webhook channels are set up by an admin; this browser's notifications are yours." />
        </Panel>
      )}
    </div>
  );
}
