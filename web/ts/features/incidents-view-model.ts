export type IncidentsCheckpointTone =
  | "neutral"
  | "success"
  | "warning"
  | "error";

export interface IncidentsCheckpointMetric {
  readonly label: string;
  readonly value: string;
}

export interface IncidentsCheckpointModel {
  readonly alertLabel: string;
  readonly canOpenTelemetry: boolean;
  readonly eventLabel: string;
  readonly focusLabel: string;
  readonly metrics: readonly IncidentsCheckpointMetric[];
  readonly nextStep: string;
  readonly scopeLabel: string;
  readonly searchLabel: string;
  readonly statusLabel: string;
  readonly statusTone: IncidentsCheckpointTone;
  readonly summary: string;
  readonly title: string;
}
