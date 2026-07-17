export type MediaCheckpointTone =
  | "neutral"
  | "success"
  | "warning"
  | "error";

export interface MediaCheckpointMetric {
  readonly label: string;
  readonly value: string;
}

export interface MediaCheckpointModel {
  readonly canOpenOverview: boolean;
  readonly focusLabel: string;
  readonly metrics: readonly MediaCheckpointMetric[];
  readonly nextStep: string;
  readonly recordingLabel: string;
  readonly searchLabel: string;
  readonly sourceLabel: string;
  readonly statusLabel: string;
  readonly statusTone: MediaCheckpointTone;
  readonly storageLabel: string;
  readonly summary: string;
  readonly title: string;
}
