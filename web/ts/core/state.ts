import type { PipelineView, ConfigData, HealthData, SystemMetrics } from '../types.js';

export interface AppState {
    config: Partial<ConfigData>;
    health: Partial<HealthData>;
    pipelines: PipelineView[];
    metrics: Partial<SystemMetrics>;
}

const _state: AppState = {
    config: {},
    health: {},
    pipelines: [],
    metrics: {},
};

/** @deprecated Prefer typed accessors: getPipelines(), getConfig(), getMetrics(), getHealth() */
export const state: AppState = _state;

/** Read-only accessors — preferred over direct `state.*` mutation */
export function getConfig(): Partial<ConfigData> { return _state.config; }
export function getHealth(): Partial<HealthData> { return _state.health; }
export function getPipelines(): readonly PipelineView[] { return _state.pipelines; }
export function getMetrics(): Partial<SystemMetrics> { return _state.metrics; }

/** Apply a partial update to the application state */
export function updateState(partial: Partial<AppState>): void {
    Object.assign(_state, partial);
}
