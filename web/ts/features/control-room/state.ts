export const controlRoomCardActionsExpanded = new Set<string>();
export const controlRoomMonitoringDrafts = new Map<string, string>();
export const controlRoomMonitoringSavePending = new Set<string>();

export let pendingMonitoringInputFocusOutputId: string | null = null;
export function setPendingMonitoringInputFocusOutputId(id: string | null): void {
  pendingMonitoringInputFocusOutputId = id;
}

export let controlRoomPlaybackIntent: "play" | "pause" = "play";
export function setControlRoomPlaybackIntent(intent: "play" | "pause"): void {
  controlRoomPlaybackIntent = intent;
}

export let controlRoomMuteIntent: "mute" | "unmute" = "mute";
export function setControlRoomMuteIntent(intent: "mute" | "unmute"): void {
  controlRoomMuteIntent = intent;
}
