const MASK_VISIBLE_PREFIX_CHARS = 20;
const MASK_VISIBLE_SUFFIX_CHARS = 5;

export function msToHHMMSS(ms: number | null): string | null {
  if (ms === null) return null;

  const totalSecs = Math.floor(ms / 1000);
  const hours = Math.floor(totalSecs / 3600);
  const mins = Math.floor((totalSecs % 3600) / 60);
  const secs = totalSecs % 60;

  return [
    hours,
    mins.toString().padStart(2, "0"),
    secs.toString().padStart(2, "0"),
  ].join(":");
}

export function maskSecret(value: unknown): string {
  const text = String(value ?? "");
  if (!text) return "";
  if (text.length <= MASK_VISIBLE_PREFIX_CHARS + MASK_VISIBLE_SUFFIX_CHARS) {
    return text;
  }
  return `${text.slice(0, MASK_VISIBLE_PREFIX_CHARS)}***${text.slice(-MASK_VISIBLE_SUFFIX_CHARS)}`;
}

export function sanitizeLogMessage(msg: unknown, redacted = true): string {
  if (!redacted) return String(msg);
  return String(msg).replace(
    /((?:https?|rtmps?|srt):\/\/[^\s'"<>()]+)/gi,
    (full, url) => maskSecret(url || full),
  );
}
