import { pipelineViewDependencies } from "../pipeline-dependencies.js";

// ── Publisher meta badge spec ──────────────────────────────────────────

interface PublisherMetaBadgeSpec {
  key: string;
  tagName: "span" | "button";
  className: string;
  text: string;
  title: string;
}

// ── DOM mutation helpers (minimal-diff) ────────────────────────────────

/** Exported for use by renderVideoTrackDetails in the main module. */
export function setTextIfChanged(target: HTMLElement, text: string): void {
  if (target.textContent !== text) {
    target.textContent = text;
  }
}

function setClassNameIfChanged(
  target: HTMLElement,
  className: string,
): void {
  if (target.className !== className) {
    target.className = className;
  }
}

function setTitleIfChanged(target: HTMLElement, title: string): void {
  if (target.title !== title) {
    target.title = title;
  }
}

// ── Publisher meta badge rendering ─────────────────────────────────────

function createPublisherMetaBadge(spec: PublisherMetaBadgeSpec): HTMLElement {
  const badge = document.createElement(spec.tagName);
  badge.dataset.metaKey = spec.key;
  if (spec.tagName === "button") {
    (badge as HTMLButtonElement).type = "button";
  }
  setClassNameIfChanged(badge, spec.className);
  setTextIfChanged(badge, spec.text);
  setTitleIfChanged(badge, spec.title);
  return badge;
}

export function syncPublisherMeta(
  container: HTMLElement,
  specs: PublisherMetaBadgeSpec[],
  pipeId: string,
): void {
  const existingBadges = new Map<string, HTMLElement>();
  Array.from(container.children).forEach((child) => {
    if (!(child instanceof HTMLElement) || !child.dataset.metaKey) return;
    existingBadges.set(child.dataset.metaKey, child);
  });

  for (const [index, spec] of specs.entries()) {
    let badge = existingBadges.get(spec.key);
    if (!badge) {
      badge = createPublisherMetaBadge(spec);
    } else {
      existingBadges.delete(spec.key);
      setClassNameIfChanged(badge, spec.className);
      setTextIfChanged(badge, spec.text);
      setTitleIfChanged(badge, spec.title);
    }

    if (spec.key === "quality" && badge instanceof HTMLButtonElement) {
      badge.onclick = () => {
        pipelineViewDependencies.openPublisherHealthModal?.(pipeId);
      };
    }

    const currentAtIndex = container.children[index] as
      | HTMLElement
      | undefined;
    if (currentAtIndex !== badge) {
      container.insertBefore(badge, currentAtIndex ?? null);
    }
  }

  for (const staleBadge of existingBadges.values()) {
    staleBadge.remove();
  }
}
