export type ControlRoomCheckpointTone =
  | "neutral"
  | "success"
  | "warning"
  | "error";

export interface ControlRoomCheckpointMetric {
  readonly label: string;
  readonly value: string;
}

export interface ControlRoomCheckpointModel {
  readonly pipelineId: string | null;
  readonly title: string;
  readonly summary: string;
  readonly statusLabel: string;
  readonly statusTone: ControlRoomCheckpointTone;
  readonly monitoredLabel: string;
  readonly missingLabel: string;
  readonly searchLabel: string;
  readonly previewLabel: string;
  readonly focusLabel: string;
  readonly nextStep: string;
  readonly canOpenPipeline: boolean;
  readonly metrics: readonly ControlRoomCheckpointMetric[];
}
