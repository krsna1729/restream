export type OutputControlIntent = "starting" | "stopping";

const pendingOutputControlIntents = new Map<string, OutputControlIntent>();
const outputControlErrors = new Map<string, string>();

function outputControlKey(pipeId: string, outId: string): string {
  return `${pipeId}:${outId}`;
}

export function beginOutputControlIntent(
  pipeId: string,
  outId: string,
  intent: OutputControlIntent,
): void {
  const key = outputControlKey(pipeId, outId);
  pendingOutputControlIntents.set(key, intent);
  outputControlErrors.delete(key);
}

export function finishOutputControlIntent(pipeId: string, outId: string): void {
  pendingOutputControlIntents.delete(outputControlKey(pipeId, outId));
}

export function getOutputControlIntent(
  pipeId: string,
  outId: string,
): OutputControlIntent | null {
  return (
    pendingOutputControlIntents.get(outputControlKey(pipeId, outId)) ?? null
  );
}

export function setOutputControlError(
  pipeId: string,
  outId: string,
  message: string | null,
): void {
  const key = outputControlKey(pipeId, outId);
  if (message) {
    outputControlErrors.set(key, message);
  } else {
    outputControlErrors.delete(key);
  }
}

export function getOutputControlError(
  pipeId: string,
  outId: string,
): string | null {
  return outputControlErrors.get(outputControlKey(pipeId, outId)) ?? null;
}
