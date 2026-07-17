export interface PipelineInspectCheckpointMetric {
  readonly label: string;
  readonly value: string;
}

export interface PipelineInspectCheckpointModel {
  readonly pipelineId: string | null;
  readonly title: string;
  readonly summary: string;
  readonly statusLabel: string;
  readonly statusTone: "success" | "warning" | "error" | "neutral";
  readonly inputLabel: string;
  readonly outputLabel: string;
  readonly attentionLabel: string;
  readonly graphLabel: string;
  readonly focusLabel: string;
  readonly nextStep: string;
  readonly canOpenPipeline: boolean;
  readonly canRunDiagnostics: boolean;
  readonly diagnosticsDisabledReason: string;
  readonly metrics: readonly PipelineInspectCheckpointMetric[];
}
