// ── Recording / file-ingest intent tracking ────────────────────────────

const pendingRecordingIntents = new Map<string, "starting" | "stopping">();
const pendingFileIngestIntents = new Map<string, "starting" | "stopping">();
const recordingLifecycleErrors = new Map<string, string>();
const fileIngestLifecycleErrors = new Map<string, string>();

function recordingIntentKey(pipeId: string): string {
  return pipeId;
}

export function getPendingRecordingIntent(
  pipeId: string,
): "starting" | "stopping" | null {
  return pendingRecordingIntents.get(recordingIntentKey(pipeId)) || null;
}

export function setPendingRecordingIntent(
  pipeId: string,
  intent: "starting" | "stopping" | null,
): void {
  const key = recordingIntentKey(pipeId);
  if (intent === null) {
    pendingRecordingIntents.delete(key);
  } else {
    pendingRecordingIntents.set(key, intent);
  }
}

export function getRecordingLifecycleError(pipeId: string): string | null {
  return recordingLifecycleErrors.get(pipeId) || null;
}

export function setRecordingLifecycleError(
  pipeId: string,
  message: string | null,
): void {
  if (message) {
    recordingLifecycleErrors.set(pipeId, message);
  } else {
    recordingLifecycleErrors.delete(pipeId);
  }
}

function fileIngestIntentKey(pipeId: string): string {
  return pipeId;
}

export function getPendingFileIngestIntent(
  pipeId: string,
): "starting" | "stopping" | null {
  return pendingFileIngestIntents.get(fileIngestIntentKey(pipeId)) || null;
}

export function setPendingFileIngestIntent(
  pipeId: string,
  intent: "starting" | "stopping" | null,
): void {
  const key = fileIngestIntentKey(pipeId);
  if (intent === null) {
    pendingFileIngestIntents.delete(key);
  } else {
    pendingFileIngestIntents.set(key, intent);
  }
}

export function getFileIngestLifecycleError(pipeId: string): string | null {
  return fileIngestLifecycleErrors.get(pipeId) || null;
}

export function setFileIngestLifecycleError(
  pipeId: string,
  message: string | null,
): void {
  if (message) {
    fileIngestLifecycleErrors.set(pipeId, message);
  } else {
    fileIngestLifecycleErrors.delete(pipeId);
  }
}
