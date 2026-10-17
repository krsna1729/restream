// What the control room's input editor sends for a pipeline input: its label,
// the source it receives media from and, for a pull input, what it presents to
// that remote origin. The engine validates every write; these functions only
// shape the operator's choices into its bodies and say what is missing before
// it is asked.
//
// The credential half is write-only: the API returns only its mechanism and a
// basic username, never a token, a password or a header value. So an edit that
// does not touch the credential sends no `auth` field at all — the engine then
// keeps the stored secret.

import type {
  PipelineInputWrite,
  PullAuthPayload,
  PullHeaderPair,
} from "../core/api-types.js";
import type {
  PipelineInput,
  PipelineInputKind,
  PullAuthKind,
  VariantRule,
} from "../types.js";

/** The source choices, in the order the editor's select lists them. */
export const SOURCE_KINDS: readonly PipelineInputKind[] = ["keyed", "hlsPull"];

export const SOURCE_LABELS: Record<PipelineInputKind, string> = {
  keyed: "Live (RTMP / SRT / HLS push)",
  hlsPull: "Pull from an HLS URL",
};

/** The renditions of a remote master playlist, in select order. */
export const VARIANT_RULES: readonly VariantRule[] = ["highestBandwidth", "lowestBandwidth"];

export const VARIANT_LABELS: Record<VariantRule, string> = {
  highestBandwidth: "Highest bandwidth",
  lowestBandwidth: "Lowest bandwidth",
};

/** The credential mechanisms, in select order. */
export const AUTH_KINDS: readonly PullAuthKind[] = [
  "none",
  "bearer",
  "cookies",
  "basic",
  "headers",
];

export const AUTH_LABELS: Record<PullAuthKind, string> = {
  none: "None",
  bearer: "Bearer token",
  cookies: "Cookies",
  basic: "Username and password",
  headers: "Custom headers",
};

/** Why `None` is usually enough: nothing is left to configure for a signed URL
 * or for the cookies an origin sets. */
export const AUTH_NONE_NOTE =
  "Usually enough: a signed URL carries its own token, and cookies the origin sets are carried automatically.";

/** The real case a cookie credential is pasted from: the three cookies of a
 * signed CloudFront URL. */
export const AUTH_COOKIES_PLACEHOLDER =
  "CloudFront-Policy=…; CloudFront-Signature=…; CloudFront-Key-Pair-Id=…";

export const AUTH_COOKIES_NOTE =
  "Sent as the Cookie header on every request. An origin that sets its own cookies needs nothing here.";

/** Header names the engine refuses as custom request headers: they belong to
 * the connection, not to the credential. */
const RESERVED_HEADER_NAMES = ["host", "content-length", "transfer-encoding", "connection"];

export interface InputDraft {
  readonly label: string;
  readonly kind: PipelineInputKind;
  readonly sourceUrl: string;
  readonly variantRule: VariantRule;
  /** The mechanism the authentication select shows. */
  readonly authKind: PullAuthKind;
  /** The stored credential is kept as it is: its secret fields are not shown
   * and no `auth` field is sent. */
  readonly keepAuth: boolean;
  readonly token: string;
  readonly cookies: string;
  readonly username: string;
  readonly password: string;
  readonly headers: readonly PullHeaderPair[];
}

/** What the editor starts with. A stored credential is never read back, so the
 * draft carries only that one exists: `keepAuth`. */
export function inputDraft(stored: PipelineInput | null): InputDraft {
  const authKind = stored?.authKind ?? "none";
  return {
    label: stored?.label ?? "",
    kind: stored?.kind ?? "keyed",
    sourceUrl: stored?.sourceUrl ?? "",
    variantRule: stored?.variantRule ?? "highestBandwidth",
    authKind,
    keepAuth: authKind !== "none",
    token: "",
    cookies: "",
    username: "",
    password: "",
    headers: [{ name: "", value: "" }],
  };
}

export type PullField = "sourceUrl" | "variantRule" | "auth";
export type AuthField = "token" | "cookies" | "username" | "password" | "headers";

/** The pull fields the editor shows: none for a keyed input, which waits for a
 * publisher on its ingest URLs instead. */
export function inputFields(draft: InputDraft): readonly PullField[] {
  return draft.kind === "hlsPull" ? ["sourceUrl", "variantRule", "auth"] : [];
}

/** The credential fields the editor shows: exactly the ones the chosen
 * mechanism needs, and none while a stored credential is kept. */
export function authFields(draft: InputDraft): readonly AuthField[] {
  if (draft.kind !== "hlsPull" || draft.keepAuth) return [];
  switch (draft.authKind) {
    case "bearer":
      return ["token"];
    case "cookies":
      return ["cookies"];
    case "basic":
      return ["username", "password"];
    case "headers":
      return ["headers"];
    case "none":
      return [];
  }
}

/** Choosing a mechanism: any other choice replaces the stored credential, and
 * `None` clears it. */
export function chooseAuthKind(draft: InputDraft, kind: PullAuthKind): InputDraft {
  return { ...draft, authKind: kind, keepAuth: false };
}

/** Show the credential fields again, to replace a stored credential with a
 * fresh one of the same mechanism. */
export function replaceAuth(draft: InputDraft): InputDraft {
  return { ...draft, keepAuth: false };
}

/** What the editor says about the credential already stored, or null when
 * there is none. A stored secret is state, never a value: only its mechanism
 * and a basic username are ever known here. */
export function storedAuthNote(stored: PipelineInput | null): string | null {
  if (!stored) return null;
  switch (stored.authKind) {
    case "bearer":
      return "Token stored";
    case "cookies":
      return "Cookies stored";
    case "basic":
      return stored.authUsername ? `Signed in as ${stored.authUsername}` : "Credentials stored";
    case "headers":
      return "Custom headers stored";
    default:
      return null;
  }
}

function isHttpUrl(url: string): boolean {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return false;
  }
  return parsed.protocol === "http:" || parsed.protocol === "https:";
}

/** RFC 7230 token characters: what `http::HeaderName` accepts, which is what
 * the engine now parses custom header names with. */
const HEADER_TOKEN = /^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/;

/** A cookie string is one line, sent as a `Cookie` header value. */
const CONTROL_CHARACTER = /[\u0000-\u001f\u007f]/;

function headerProblem(headers: readonly PullHeaderPair[]): string | null {
  const named = headers.filter((header) => header.name.trim());
  if (named.length === 0) return "Add a header, or choose None.";
  for (const header of named) {
    const name = header.name.trim();
    if (!HEADER_TOKEN.test(name)) return `"${name}" is not a valid header name.`;
    if (RESERVED_HEADER_NAMES.includes(name.toLowerCase())) {
      return `"${name}" belongs to the connection; it cannot be set here.`;
    }
  }
  return null;
}

/** The source lines a deck row lists: a pull input names the remote URL it
 * fetches and no ingest URL (it accepts no pushes), while a keyed input lists
 * the publish URLs of a backup — what it has always shown. */
export function inputSourceLines(input: PipelineInput): string[] {
  if (input.kind === "hlsPull") return input.sourceUrl ? [input.sourceUrl] : [];
  if (input.role !== "backup") return [];
  return [input.ingestUrls?.rtmp, input.ingestUrls?.srt, input.ingestUrls?.hls].filter(
    (url): url is string => Boolean(url),
  );
}

/** Why the engine would refuse this input, or null. */
export function inputProblem(draft: InputDraft): string | null {
  if (!draft.label.trim()) return "An input needs a label.";
  if (draft.kind !== "hlsPull") return null;
  const url = draft.sourceUrl.trim();
  if (!url) return "A pull input needs the HLS source URL.";
  if (!isHttpUrl(url)) return "The HLS source URL must be an absolute http or https URL.";
  if (draft.keepAuth) return null;
  switch (draft.authKind) {
    case "bearer":
      return draft.token.trim() ? null : "A bearer token needs a token.";
    case "cookies": {
      const cookies = draft.cookies.trim();
      if (!cookies) return "The cookie credential needs the cookie string.";
      return CONTROL_CHARACTER.test(cookies)
        ? "A cookie string is one line, without control characters."
        : null;
    }
    case "basic":
      if (!draft.username.trim()) return "Basic auth needs a username.";
      return draft.password ? null : "Basic auth needs a password.";
    case "headers":
      return headerProblem(draft.headers);
    case "none":
      return null;
  }
}

/** The credential to send, or null to leave the stored one alone. */
function authWrite(draft: InputDraft, stored: PipelineInput | null): PullAuthPayload | null {
  const storedCredential = stored !== null && stored.authKind !== "none";
  if (draft.kind !== "hlsPull") {
    // A keyed input carries no credential, and the engine refuses a keyed
    // source that still has one: say explicitly that a stored one is gone, so
    // turning a pull input back into a live one is accepted rather than 400ed.
    return storedCredential ? { kind: "none" } : null;
  }
  if (draft.keepAuth && storedCredential) return null;
  switch (draft.authKind) {
    case "bearer":
      return { kind: "bearer", token: draft.token.trim() };
    case "cookies":
      return { kind: "cookies", cookies: draft.cookies.trim() };
    case "basic":
      return { kind: "basic", username: draft.username.trim(), password: draft.password };
    case "headers":
      return {
        kind: "headers",
        headers: draft.headers
          .filter((header) => header.name.trim())
          .map((header) => ({ name: header.name.trim(), value: header.value })),
      };
    case "none":
      return { kind: "none" };
  }
}

/** The create body (`stored` null) or the update body. An edit that leaves the
 * stored credential alone carries no `auth` field, so the engine keeps a
 * secret no endpoint ever returns. */
export function inputWrite(draft: InputDraft, stored: PipelineInput | null): PipelineInputWrite {
  const pull = draft.kind === "hlsPull";
  const body: PipelineInputWrite = {
    label: draft.label.trim(),
    kind: draft.kind,
    sourceUrl: pull ? draft.sourceUrl.trim() : null,
    variantRule: draft.variantRule,
  };
  const auth = authWrite(draft, stored);
  return auth === null ? body : { ...body, auth };
}
