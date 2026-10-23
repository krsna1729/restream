// API calls for the admin surfaces: the team, the audit log, notifications,
// schedules and share links. Split from api.ts by size; the same rules
// apply (every route literal lives in one of these two modules).

import { apiRequest } from "./api.js";
import type {
  AuditEntry,
  NotifyChannel,
  NotifyChannelDraft,
  NotifyDelivery,
  NotifySeverity,
  PushSubscriptionRecord,
  Role,
  Schedule,
  ScheduleDraft,
  SessionPrincipal,
  ShareLink,
  TeamMember,
} from "./api-types.js";

/** Who this session is; null (quietly) when it has no principal. */
async function getSession(): Promise<SessionPrincipal | null> {
  return apiRequest<SessionPrincipal>("/api/v1/auth/me", { silentStatuses: [404] });
}

/** View the dashboard as a lower role (the server enforces it), or null to
 * return to one's own. */
async function setViewAs(role: Role | null): Promise<SessionPrincipal | null> {
  return apiRequest<SessionPrincipal>("/api/v1/auth/view-as", {
    method: "POST",
    body: { role },
  });
}

async function listTeam(): Promise<TeamMember[] | null> {
  const body = await apiRequest<{ users: TeamMember[] }>("/api/v1/users", { silentStatuses: [403] });
  return body?.users ?? null;
}

async function createTeamMember(member: {
  username: string;
  password: string;
  role: Role;
}): Promise<TeamMember | null> {
  return apiRequest<TeamMember>("/api/v1/users", { method: "POST", body: member });
}

async function updateTeamMember(
  id: string,
  change: { role?: Role; disabled?: boolean; password?: string },
): Promise<TeamMember | null> {
  return apiRequest<TeamMember>(`/api/v1/users/${encodeURIComponent(id)}`, {
    method: "PATCH",
    body: change,
  });
}

async function deleteTeamMember(id: string): Promise<boolean> {
  const body = await apiRequest<{ deleted: boolean }>(`/api/v1/users/${encodeURIComponent(id)}`, {
    method: "DELETE",
  });
  return body?.deleted === true;
}

/** Newest first; `before` pages back from an entry id. */
async function getAudit(before?: number, limit = 100): Promise<AuditEntry[] | null> {
  const query = new URLSearchParams({ limit: String(limit) });
  if (before !== undefined) query.set("before", String(before));
  const body = await apiRequest<{ entries: AuditEntry[] }>(`/api/v1/audit?${query}`, {
    silentStatuses: [403],
  });
  return body?.entries ?? null;
}

async function listNotifyChannels(): Promise<NotifyChannel[] | null> {
  const body = await apiRequest<{ channels: NotifyChannel[] }>("/api/v1/notifications/channels", {
    silentStatuses: [403],
  });
  return body?.channels ?? null;
}

/** A new channel; a webhook's signing secret is returned only here. */
async function createNotifyChannel(
  draft: NotifyChannelDraft,
): Promise<{ channel: NotifyChannel; secret: string | null } | null> {
  return apiRequest("/api/v1/notifications/channels", { method: "POST", body: draft });
}

async function updateNotifyChannel(id: string, draft: NotifyChannelDraft): Promise<NotifyChannel | null> {
  return apiRequest<NotifyChannel>(`/api/v1/notifications/channels/${encodeURIComponent(id)}`, {
    method: "PUT",
    body: draft,
  });
}

async function deleteNotifyChannel(id: string): Promise<boolean> {
  const body = await apiRequest<{ deleted: boolean }>(
    `/api/v1/notifications/channels/${encodeURIComponent(id)}`,
    { method: "DELETE" },
  );
  return body?.deleted === true;
}

/** Sends a test now; the delivery says how it ended. */
async function testNotifyChannel(id: string): Promise<NotifyDelivery | null> {
  return apiRequest<NotifyDelivery>(`/api/v1/notifications/channels/${encodeURIComponent(id)}/test`, {
    method: "POST",
  });
}

async function getNotifyChannelSecret(id: string): Promise<string | null> {
  const body = await apiRequest<{ secret: string | null }>(
    `/api/v1/notifications/channels/${encodeURIComponent(id)}/secret`,
  );
  return body?.secret ?? null;
}

async function listNotifyDeliveries(limit = 100): Promise<NotifyDelivery[] | null> {
  const body = await apiRequest<{ deliveries: NotifyDelivery[] }>(
    `/api/v1/notifications/deliveries?limit=${limit}`,
    { silentStatuses: [403] },
  );
  return body?.deliveries ?? null;
}

async function getPushPublicKey(): Promise<string | null> {
  const body = await apiRequest<{ publicKey: string }>("/api/v1/notifications/push/key", {
    silentStatuses: [404],
  });
  return body?.publicKey ?? null;
}

async function listMyPushSubscriptions(): Promise<PushSubscriptionRecord[] | null> {
  const body = await apiRequest<{ subscriptions: PushSubscriptionRecord[] }>(
    "/api/v1/notifications/push/subscriptions",
    { silentStatuses: [404] },
  );
  return body?.subscriptions ?? null;
}

/** Registers this browser's push subscription (`PushSubscription.toJSON()`). */
async function savePushSubscription(
  subscription: { endpoint: string; keys: { p256dh: string; auth: string } },
  minSeverity: NotifySeverity,
): Promise<PushSubscriptionRecord | null> {
  return apiRequest<PushSubscriptionRecord>("/api/v1/notifications/push/subscriptions", {
    method: "POST",
    body: { endpoint: subscription.endpoint, keys: subscription.keys, minSeverity },
  });
}

async function deletePushSubscription(id: string): Promise<boolean> {
  const body = await apiRequest<{ deleted: boolean }>(
    `/api/v1/notifications/push/subscriptions/${encodeURIComponent(id)}`,
    { method: "DELETE" },
  );
  return body?.deleted === true;
}

async function testPush(): Promise<NotifyDelivery[] | null> {
  const body = await apiRequest<{ deliveries: NotifyDelivery[] }>("/api/v1/notifications/push/test", {
    method: "POST",
  });
  return body?.deliveries ?? null;
}

async function listSchedules(): Promise<Schedule[] | null> {
  const body = await apiRequest<{ schedules: Schedule[] }>("/api/v1/schedules", { silentStatuses: [404] });
  return body?.schedules ?? null;
}

async function createSchedule(draft: ScheduleDraft): Promise<Schedule | null> {
  return apiRequest<Schedule>("/api/v1/schedules", { method: "POST", body: draft });
}

async function updateSchedule(id: string, draft: ScheduleDraft): Promise<Schedule | null> {
  return apiRequest<Schedule>(`/api/v1/schedules/${encodeURIComponent(id)}`, { method: "PUT", body: draft });
}

async function deleteSchedule(id: string): Promise<boolean> {
  const body = await apiRequest<{ deleted: boolean }>(`/api/v1/schedules/${encodeURIComponent(id)}`, {
    method: "DELETE",
  });
  return body?.deleted === true;
}

/** Runs the action now (the times stay); `result` says what it did. */
async function runSchedule(id: string): Promise<{ ok: boolean; result?: string } | null> {
  return apiRequest(`/api/v1/schedules/${encodeURIComponent(id)}/run`, { method: "POST" });
}

async function listShareLinks(pipelineId: string): Promise<ShareLink[] | null> {
  const body = await apiRequest<{ links: ShareLink[] }>(
    `/api/v1/pipelines/${encodeURIComponent(pipelineId)}/share-links`,
    { silentStatuses: [403] },
  );
  return body?.links ?? null;
}

/** `lifetimeHours` null: until revoked. */
async function createShareLink(
  pipelineId: string,
  label: string,
  lifetimeHours: number | null,
): Promise<ShareLink | null> {
  return apiRequest<ShareLink>(`/api/v1/pipelines/${encodeURIComponent(pipelineId)}/share-links`, {
    method: "POST",
    body: { label, lifetimeHours },
  });
}

async function revokeShareLink(id: string): Promise<boolean> {
  const body = await apiRequest<{ deleted: boolean }>(`/api/v1/share-links/${encodeURIComponent(id)}`, {
    method: "DELETE",
  });
  return body?.deleted === true;
}

export {
  getSession,
  setViewAs,
  listTeam,
  createTeamMember,
  updateTeamMember,
  deleteTeamMember,
  getAudit,
  listNotifyChannels,
  createNotifyChannel,
  updateNotifyChannel,
  deleteNotifyChannel,
  testNotifyChannel,
  getNotifyChannelSecret,
  listNotifyDeliveries,
  getPushPublicKey,
  listMyPushSubscriptions,
  savePushSubscription,
  deletePushSubscription,
  testPush,
  listSchedules,
  createSchedule,
  updateSchedule,
  deleteSchedule,
  runSchedule,
  listShareLinks,
  createShareLink,
  revokeShareLink,
};
