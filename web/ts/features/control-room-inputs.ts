import { getPipelineInputs, promotePipelineInput } from "../core/api.js";
import type { PipelineInput } from "../types.js";
import { buildPipelineInputPreviewUrl } from "./input-preview.js";
import type { ControlRoomCardDescriptor } from "./control-room-types.js";
import {
  pipelineInputStatusLabel,
  pipelineInputSubtitle,
} from "./pipeline-inputs-view-model.js";

const STATUS_TTL_MS = 2_000;
const promotionPending = new Set<string>();
const inputCache = new Map<
  string,
  { expiresAt: number; inputs: PipelineInput[] }
>();
const inputRequests = new Set<string>();

export function buildControlRoomInputCard(
  input: PipelineInput,
): ControlRoomCardDescriptor {
  const previewUrl = buildPipelineInputPreviewUrl(input.id);
  return {
    id: `input:${input.id}`,
    title: input.label,
    subtitle: pipelineInputSubtitle(input),
    mediaUrl: input.enabled && input.runtime.connected ? previewUrl : null,
    loadOnDemand: true,
    emptyMessage: input.enabled
      ? input.runtime.connected
        ? "Waiting for preview segments."
        : "Input is offline."
      : "Input is disabled.",
    openUrl: previewUrl,
    copyUrl: previewUrl,
    editable: false,
    outputId: null,
    pipelineId: input.pipelineId,
    monitoringUrl: previewUrl,
    statusLabel: pipelineInputStatusLabel(input),
    promoteInputId: input.enabled && !input.selected ? input.id : null,
  };
}

export function controlRoomInputs(
  pipelineId: string,
  onRefresh: () => void,
): PipelineInput[] | null {
  refreshInputs(pipelineId, onRefresh);
  return inputCache.get(pipelineId)?.inputs ?? null;
}

export function isControlRoomInputPromotionPending(inputId: string): boolean {
  return promotionPending.has(inputId);
}

export async function promoteControlRoomInput(
  pipelineId: string,
  inputId: string,
  onRefresh: () => void,
): Promise<void> {
  if (promotionPending.has(inputId)) return;
  promotionPending.add(inputId);
  onRefresh();
  try {
    const response = await promotePipelineInput(pipelineId, inputId);
    if (response) inputCache.delete(pipelineId);
  } finally {
    promotionPending.delete(inputId);
    refreshInputs(pipelineId, onRefresh);
    onRefresh();
  }
}

function refreshInputs(pipelineId: string, onRefresh: () => void): void {
  const cached = inputCache.get(pipelineId);
  if (
    inputRequests.has(pipelineId) ||
    (cached && cached.expiresAt > Date.now())
  ) {
    return;
  }
  inputRequests.add(pipelineId);
  void getPipelineInputs(pipelineId)
    .then((response) => {
      if (!response) return;
      inputCache.set(pipelineId, {
        expiresAt: Date.now() + STATUS_TTL_MS,
        inputs: response.inputs,
      });
    })
    .finally(() => {
      inputRequests.delete(pipelineId);
      onRefresh();
    });
}
