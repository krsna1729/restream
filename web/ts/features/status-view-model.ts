export type StatusCheckpointTone =
  | "neutral"
  | "success"
  | "warning"
  | "error";

export interface StatusCheckpointMetric {
  readonly label: string;
  readonly value: string;
}

export interface StatusCheckpointModel {
  readonly activityLabel: string;
  readonly buildLabel: string;
  readonly canOpenTelemetry: boolean;
  readonly focusLabel: string;
  readonly logLabel: string;
  readonly metrics: readonly StatusCheckpointMetric[];
  readonly nextStep: string;
  readonly searchLabel: string;
  readonly statusLabel: string;
  readonly statusTone: StatusCheckpointTone;
  readonly summary: string;
  readonly title: string;
}
