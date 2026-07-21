export type UnknownRecord = Record<string, unknown>;

export function isRecord(value: unknown): value is UnknownRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function stringOrNull(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

export function finiteNonNegativeNumber(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) && value >= 0
    ? value
    : null;
}

export function nonNegativeNumberOrZero(value: unknown): number {
  return finiteNonNegativeNumber(value) ?? 0;
}

export function timestampMs(value: unknown): number | null {
  if (typeof value !== "string" || value.length === 0) return null;
  const timestamp = new Date(value).getTime();
  return Number.isFinite(timestamp) ? timestamp : null;
}
