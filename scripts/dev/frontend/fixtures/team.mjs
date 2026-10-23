// A synthetic team for the fixture server: members across the three roles,
// a session that can view as a lower role (and is refused like the engine
// refuses), and an audit log of plausible changes against the deployment.

import { rng } from "./density.mjs";

const ROLES = ["viewer", "operator", "admin"];
const rank = (role) => ROLES.indexOf(role);

const FIRST = ["ana", "ben", "chi", "dev", "eli", "fay", "gus", "hana", "ivo", "jun", "kai", "lea", "mo", "nia", "oz", "pia", "raj", "sol", "tia", "uma", "vik", "wen", "yas", "zoe"];

/** The least role a request needs; mirrors `application/team.rs`. */
export function requiredRole(method, path) {
  if (path.startsWith("/api/v1/notifications/push/")) return "viewer";
  if (["GET", "HEAD", "OPTIONS"].includes(method)) {
    if (/^\/api\/v1\/(users|audit|security\/|notifications)/.test(path)) return "admin";
    if (path === "/api/v1/settings" || path.startsWith("/api/v1/stream-keys")) return "operator";
    return "viewer";
  }
  if (path.startsWith("/api/v1/auth/")) return "viewer";
  if (path === "/api/v1/settings" || /^\/api\/v1\/(security\/|users|encodings\/custom|notifications)/.test(path)) return "admin";
  return "operator";
}

export function createTeam({ seed = 7, members = 24, outputs = [], nowMs = Date.now() } = {}) {
  const random = rng(seed * 31 + 5);
  const users = [{ id: "admin", username: "admin", role: "admin", createdAtMs: 0, disabled: false }];
  for (let index = 0; index < members; index += 1) {
    const role = index < 3 ? "admin" : index < 12 ? "operator" : "viewer";
    users.push({
      id: `u_${(index + 1).toString(16).padStart(4, "0")}`,
      username: `${FIRST[index % FIRST.length]}${index >= FIRST.length ? index : ""}.${role === "viewer" ? "noc" : "ops"}`,
      role,
      createdAtMs: nowMs - random.int(2, 400) * 86_400_000,
      disabled: random.chance(0.08),
    });
  }
  let viewAs = null;

  const audit = [];
  const actors = users.filter((user) => user.role !== "viewer");
  let atMs = nowMs - 6 * 3_600_000;
  for (let id = 1; id <= 640; id += 1) {
    atMs += random.int(5, 60) * 1000;
    const output = outputs.length ? random.pick(outputs) : { id: "o1", pipelineId: "p1" };
    const actor = random.pick(actors);
    const kind = random.next();
    const [method, path] =
      kind < 0.4
        ? ["POST", `/api/v1/pipelines/${output.pipelineId}/outputs/${output.id}/${random.chance(0.5) ? "start" : "stop"}`]
        : kind < 0.55
          ? ["PATCH", `/api/v1/pipelines/${output.pipelineId}/outputs/${output.id}`]
          : kind < 0.65
            ? ["POST", `/api/v1/pipelines/${output.pipelineId}/inputs/in-backup/promote`]
            : kind < 0.75
              ? ["POST", `/api/v1/pipelines/${output.pipelineId}/recording/start`]
              : kind < 0.8
                ? ["PATCH", "/api/v1/settings"]
                : kind < 0.9
                  ? ["POST", "/api/v1/auth/view-as"]
                  : ["POST", "/api/v1/pipelines"];
    const viewer = random.chance(0.06) ? random.pick(users.filter((user) => user.role === "viewer")) : null;
    const who = viewer ?? actor;
    const refused = rank(who.role) < rank(requiredRole(method, path));
    audit.push({
      id,
      atMs,
      username: who.username,
      role: who.role,
      method,
      path,
      status: refused ? 403 : random.chance(0.03) ? 409 : 200,
    });
  }
  audit.reverse();

  const me = () => {
    const effectiveRole = viewAs ?? "admin";
    return { userId: "admin", username: "admin", role: "admin", viewAs, effectiveRole };
  };

  /** Handles a team route, or returns undefined for any other path. */
  function handle(method, path, query, body) {
    const effective = me().effectiveRole;
    const needed = requiredRole(method, path);
    if (path.startsWith("/api/v1/") && rank(effective) < rank(needed)) {
      return {
        status: 403,
        body: {
          error: `This needs the ${needed} role; this session acts as ${effective}.`,
          requiredRole: needed,
          effectiveRole: effective,
        },
      };
    }
    if (path === "/api/v1/auth/me" && method === "GET") return { body: me() };
    if (path === "/api/v1/auth/view-as" && method === "POST") {
      viewAs = body?.role && rank(body.role) < rank("admin") ? body.role : null;
      return { body: me() };
    }
    if (path === "/api/v1/users" && method === "GET") return { body: { users } };
    if (path === "/api/v1/users" && method === "POST") {
      if (users.some((user) => user.username === body?.username)) {
        return { status: 409, body: { error: `${body.username} is already a member.` } };
      }
      const user = { id: `u_${Date.now().toString(16)}`, username: body.username, role: body.role, createdAtMs: Date.now(), disabled: false };
      users.push(user);
      return { status: 201, body: user };
    }
    const one = path.match(/^\/api\/v1\/users\/([^/]+)$/);
    if (one) {
      const index = users.findIndex((user) => user.id === decodeURIComponent(one[1]));
      if (index < 0) return { status: 404, body: { error: "no such member" } };
      if (method === "PATCH") {
        const { role, disabled } = body ?? {};
        users[index] = { ...users[index], ...(role ? { role } : {}), ...(disabled === undefined ? {} : { disabled }) };
        return { body: users[index] };
      }
      if (method === "DELETE") {
        users.splice(index, 1);
        return { body: { deleted: true } };
      }
    }
    if (path === "/api/v1/audit" && method === "GET") {
      const limit = Number(query.get("limit") ?? 100);
      const before = query.get("before");
      const page = audit.filter((entry) => before === null || entry.id < Number(before)).slice(0, limit);
      return { body: { entries: page } };
    }
    return undefined;
  }

  function record(method, path, status) {
    const who = me();
    audit.unshift({ id: (audit[0]?.id ?? 0) + 1, atMs: Date.now(), username: who.username, role: who.effectiveRole, method, path, status });
  }

  return { handle, record };
}
