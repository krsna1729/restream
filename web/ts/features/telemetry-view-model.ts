export type TelemetryCheckpointTone =
  | "neutral"
  | "success"
  | "warning"
  | "error";

export interface TelemetryCheckpointMetric {
  readonly label: string;
  readonly value: string;
}

export interface TelemetryCheckpointModel {
  readonly canOpenStatus: boolean;
  readonly counterLabel: string;
  readonly egressLabel: string;
  readonly focusLabel: string;
  readonly metrics: readonly TelemetryCheckpointMetric[];
  readonly nextStep: string;
  readonly pipelineLabel: string;
  readonly searchLabel: string;
  readonly statusLabel: string;
  readonly statusTone: TelemetryCheckpointTone;
  readonly summary: string;
  readonly title: string;
}
