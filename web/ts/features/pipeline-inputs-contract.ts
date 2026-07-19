import type {
  PipelineInputMutationResponse,
  PipelineInputPromotionResponse,
  PipelineInputsResponse,
} from "../types.js";

export interface PipelineInputsPanelActions {
  readonly copyValue: (value: string) => Promise<void>;
  readonly createInput: (
    pipelineId: string,
    label: string,
  ) => Promise<PipelineInputMutationResponse | null>;
  readonly deleteInput: (
    pipelineId: string,
    inputId: string,
  ) => Promise<{ deleted: boolean } | null>;
  readonly listInputs: (
    pipelineId: string,
  ) => Promise<PipelineInputsResponse | null>;
  readonly promoteInput: (
    pipelineId: string,
    inputId: string,
  ) => Promise<PipelineInputPromotionResponse | null>;
  readonly updateInput: (
    pipelineId: string,
    inputId: string,
    data: { label?: string; enabled?: boolean },
  ) => Promise<PipelineInputMutationResponse | null>;
}
