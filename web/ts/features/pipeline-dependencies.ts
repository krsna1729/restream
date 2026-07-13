import type { PipelineFileIngestState } from "../types.js";

export interface PipelineViewDependencies {
  openPipelineHistoryModal: ((pipeId: string, pipeName: string) => void) | null;
  openPublisherHealthModal: ((pipeId: string) => void) | null;
  isOutputToggleBusy: ((pipeId: string, outId: string) => boolean) | null;
  startOutBtn:
    | ((
        pipeId: string,
        outId: string,
        button: HTMLButtonElement | null,
      ) => Promise<void>)
    | null;
  stopOutBtn:
    | ((
        pipeId: string,
        outId: string,
        button: HTMLButtonElement | null,
      ) => Promise<void>)
    | null;
  openOutputHistoryModal:
    ((pipeId: string, outId: string, outName: string) => void) | null;
  editOutBtn: ((pipeId: string, outId: string) => void) | null;
  deleteOutBtn: ((pipeId: string, outId: string) => void) | null;
  refreshDashboard: (() => Promise<void>) | null;
  refreshDashboardRuntime: (() => Promise<void>) | null;
  awaitDashboardRuntimeMutationConvergence:
    | ((predicate?: (() => boolean) | null) => Promise<void>)
    | null;
  updateDashboardPipelineFileIngestState:
    | ((
        pipelineId: string,
        fileIngest: PipelineFileIngestState | null,
      ) => void)
    | null;
  updateDashboardPipelineRecordingState:
    | (
        (
          pipelineId: string,
          recording: { enabled: boolean; active: boolean },
        ) => void
      )
    | null;
  openDiagnosticsModal: ((pipeId: string) => void) | null;
  openGraphExplorer: ((pipeId: string) => void) | null;
  openOutputMonitoringUrl: ((url: string | null | undefined) => void) | null;
}

export const pipelineViewDependencies: PipelineViewDependencies = {
  openPipelineHistoryModal: null,
  openPublisherHealthModal: null,
  isOutputToggleBusy: null,
  startOutBtn: null,
  stopOutBtn: null,
  openOutputHistoryModal: null,
  editOutBtn: null,
  deleteOutBtn: null,
  refreshDashboard: null,
  refreshDashboardRuntime: null,
  awaitDashboardRuntimeMutationConvergence: null,
  updateDashboardPipelineFileIngestState: null,
  updateDashboardPipelineRecordingState: null,
  openDiagnosticsModal: null,
  openGraphExplorer: null,
  openOutputMonitoringUrl: null,
};

export function setPipelineViewDependencies(
  dependencies: Partial<PipelineViewDependencies>,
): void {
  Object.assign(pipelineViewDependencies, dependencies || {});
}
