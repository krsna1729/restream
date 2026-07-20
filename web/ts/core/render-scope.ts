/**
 * Tracks which DOM container a feature module is currently rendering into and
 * a generation counter that bumps whenever that target changes. Async work
 * (fetch, prompt, upload) captures a token before awaiting and checks
 * `isCurrent` after, so a stale response from a container the feature no
 * longer owns is dropped instead of repainting the wrong host.
 */
export interface RenderScopeToken {
  readonly containerId: string;
  readonly generation: number;
}

export class RenderScope {
  private containerId: string;
  private generation = 0;

  constructor(defaultContainerId: string) {
    this.containerId = defaultContainerId;
  }

  current(): string {
    return this.containerId;
  }

  token(): RenderScopeToken {
    return { containerId: this.containerId, generation: this.generation };
  }

  isCurrent(token: RenderScopeToken): boolean {
    return (
      this.containerId === token.containerId &&
      this.generation === token.generation
    );
  }

  /** Bumps the generation without changing the container id. */
  invalidate(): void {
    this.generation += 1;
  }

  /** Returns true when the container id changed (and the generation bumped). */
  setContainerId(containerId: string): boolean {
    if (this.containerId === containerId) return false;
    this.containerId = containerId;
    this.generation += 1;
    return true;
  }
}
