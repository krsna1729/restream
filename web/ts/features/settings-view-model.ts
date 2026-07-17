export type SettingsCheckpointTone =
  | "neutral"
  | "success"
  | "warning"
  | "error";

export interface SettingsCheckpointMetric {
  readonly label: string;
  readonly value: string;
}

export interface SettingsCheckpointModel {
  readonly authLabel: string;
  readonly canOpenStatus: boolean;
  readonly focusLabel: string;
  readonly metrics: readonly SettingsCheckpointMetric[];
  readonly nextStep: string;
  readonly profileLabel: string;
  readonly searchLabel: string;
  readonly sectionLabel: string;
  readonly securityLabel: string;
  readonly statusLabel: string;
  readonly statusTone: SettingsCheckpointTone;
  readonly summary: string;
  readonly title: string;
}
