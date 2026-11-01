// What the command palette can find and how it ranks a query: workspaces,
// pipelines, destinations, open incidents and destination commands from
// what the page already holds. Pure, so ranking is tested without a DOM;
// with ten thousand destinations a keystroke ranks in tens of milliseconds.
//
// A command never runs from here: it opens Changes with the change drafted
// and judged, where it is created as an operation and approved by role.
// Matching is loose but not a language model: terms in any order, verb
// synonyms, and a typo per word.

import type { OperatorAlert } from "../../core/api-types.js";
import type { OutputState, RuntimeState } from "../../core/generated/runtime-state.js";
import { WORKSPACE_LABELS, WORKSPACES } from "../route.js";
import type { ChangeIntent, Place, Workspace } from "../route.js";

export type PaletteKind = "workspace" | "pipeline" | "destination" | "incident" | "command" | "action";

export interface PaletteEntry {
  readonly id: string;
  readonly kind: PaletteKind;
  readonly label: string;
  /** A second line: protocol, pipeline, state. */
  readonly detail: string;
  /** Extra words that match but are not shown (ids, platform). */
  readonly keywords: string;
  readonly place?: Place;
  readonly action?: string;
}

export const PALETTE_ACTIONS: ReadonlyArray<{ id: string; label: string; detail: string }> = [
  { id: "theme:dark", label: "Theme: dark", detail: "Switch to the dark theme" },
  { id: "theme:light", label: "Theme: light", detail: "Switch to the light theme" },
  { id: "theme:system", label: "Theme: follow system", detail: "Match the operating system" },
  { id: "density:compact", label: "Density: compact", detail: "More rows per screen" },
  { id: "density:comfortable", label: "Density: comfortable", detail: "Larger type and targets" },
];

/** The destination commands Changes can draft, each with the words an
 * operator might use for it, and when it applies. */
const COMMANDS: ReadonlyArray<{
  kind: ChangeIntent["kind"];
  verb: string;
  synonyms: string;
  applies: (output: OutputState) => boolean;
}> = [
  { kind: "startOutput", verb: "Start", synonyms: "begin resume launch enable run restart", applies: (output) => output.desired !== "running" },
  { kind: "stopOutput", verb: "Stop", synonyms: "halt pause kill end disable", applies: (output) => output.desired === "running" },
  { kind: "removeOutput", verb: "Remove", synonyms: "delete drop retire", applies: () => true },
];

export function paletteEntries(
  state: RuntimeState | null,
  alerts: readonly OperatorAlert[],
  currentPipeline: string | null,
  /** The workspaces this session may open; all by default. */
  canOpen: (workspace: Workspace) => boolean = () => true,
): PaletteEntry[] {
  const entries: PaletteEntry[] = WORKSPACES.filter(canOpen).map((workspace) => ({
    id: `w:${workspace}`,
    kind: "workspace",
    label: WORKSPACE_LABELS[workspace],
    detail: "Workspace",
    keywords: workspace,
    place: { workspace, pipelineId: currentPipeline },
  }));
  const pipelineNames = new Map<string, string>();
  for (const pipeline of state?.pipelines ?? []) {
    pipelineNames.set(pipeline.id, pipeline.name);
    entries.push({
      id: `p:${pipeline.id}`,
      kind: "pipeline",
      label: pipeline.name,
      detail: pipeline.ingest ? `${pipeline.ingest.protocol.toUpperCase()} live` : "no source",
      keywords: pipeline.id,
      place: { workspace: "operate", pipelineId: pipeline.id },
    });
  }
  const commands = canOpen("changes");
  for (const output of state?.outputs ?? []) {
    const pipelineName = pipelineNames.get(output.pipelineId) ?? output.pipelineId;
    entries.push({
      id: `o:${output.id}`,
      kind: "destination",
      label: output.name,
      detail: `${pipelineName} · ${output.protocol.toUpperCase()} · ${output.diagnosis ? "degraded" : output.desired === "running" ? output.convergence : "stopped"}`,
      keywords: `${output.id} ${output.protocol}`,
      place: { workspace: "operate", pipelineId: output.pipelineId },
    });
    if (!commands) continue;
    for (const command of COMMANDS.filter((candidate) => candidate.applies(output))) {
      entries.push({
        id: `c:${command.kind}:${output.id}`,
        kind: "command",
        label: `${command.verb} ${output.name}`,
        detail: `${pipelineName} · drafted in Changes; nothing runs until it is approved`,
        keywords: `${command.synonyms} destination output ${output.id} ${output.protocol}`,
        place: {
          workspace: "changes",
          pipelineId: output.pipelineId,
          change: { kind: command.kind, outputId: output.id },
        },
      });
    }
  }
  for (const alert of alerts) {
    entries.push({
      id: `a:${alert.id}`,
      kind: "incident",
      label: alert.title,
      detail: `${alert.severity} · ${alert.cause}`,
      keywords: alert.id,
      place: { workspace: "incidents", pipelineId: alert.pipelineId ?? currentPipeline },
    });
  }
  for (const action of PALETTE_ACTIONS) {
    entries.push({ id: `x:${action.id}`, kind: "action", label: action.label, detail: action.detail, keywords: "", action: action.id });
  }
  return entries;
}

/** Whether a word is within a typo of `term`: one edit for terms of four to
 * seven characters, two from eight; shorter terms must match exactly. */
function nearTo(term: string): (word: string) => boolean {
  if (term.length < 4) return () => false;
  const budget = term.length >= 8 ? 2 : 1;
  return (word) => Math.abs(word.length - term.length) <= budget && editDistance(term, word, budget) <= budget;
}

const KIND_BOOST: Record<PaletteKind, number> = { workspace: 2, incident: 1.5, pipeline: 1, destination: 0, command: 0, action: 0 };

/** Each command's verb and synonyms, as a query spells them. */
const VERBS = COMMANDS.map((command) => ({
  kind: command.kind,
  words: [command.verb.toLowerCase(), ...command.synonyms.split(" ")],
}));

/** The commands a query asks for: a term is the verb or a synonym, starts
 * one (three letters or more), or is a typo of one. A query that asks for
 * none looks only for places, so commands are not scored at all. */
function askedCommands(terms: readonly string[], near: ReadonlyArray<(word: string) => boolean>): Set<ChangeIntent["kind"]> {
  const asked = new Set<ChangeIntent["kind"]>();
  for (const { kind, words } of VERBS) {
    const named = terms.some((term, index) =>
      words.some((verb) => verb === term || (term.length >= 3 && verb.startsWith(term)) || near[index](verb)),
    );
    if (named) asked.add(kind);
  }
  return asked;
}

/** An entry's text, lowercased for one query. */
interface Text {
  readonly entry: PaletteEntry;
  readonly label: string;
  readonly rest: string;
}

/** The words entries are named by (a command's name is its verb and its
 * destination's name), per entry list: the palette ranks one list on every
 * keystroke and makes a new one only when the runtime state changes. */
const VOCABULARY = new WeakMap<readonly PaletteEntry[], readonly string[]>();

function vocabulary(entries: readonly PaletteEntry[]): readonly string[] {
  let words = VOCABULARY.get(entries);
  if (!words) {
    const named = entries
      .filter((entry) => entry.kind !== "command")
      .map((entry) => entry.label)
      .join(" ")
      .toLowerCase()
      .split(/[^\p{L}\p{N}]+/u);
    words = [...new Set([...named, ...VERBS.flatMap((verb) => verb.words)])];
    VOCABULARY.set(entries, words);
  }
  return words;
}

/** The words a typo away from each term that no entry contains; none for a
 * term some entry does contain (it is spelt as meant). */
function corrections(
  entries: readonly PaletteEntry[],
  texts: readonly Text[],
  terms: readonly string[],
  near: ReadonlyArray<(word: string) => boolean>,
): string[][] {
  return terms.map((term, index) =>
    texts.some((text) => text.label.includes(term) || text.rest.includes(term))
      ? []
      : vocabulary(entries).filter(near[index]),
  );
}

/** How well an entry matches every term; 0 is no match. A misspelt term
 * matches an entry holding one of its corrections, for less. */
function score(text: Text, terms: readonly string[], fixes: readonly string[][]): number {
  const { label, rest } = text;
  let total = 0;
  for (let index = 0; index < terms.length; index++) {
    const term = terms[index];
    if (label.startsWith(term)) total += 8;
    else if (label.includes(` ${term}`)) total += 5;
    else if (label.includes(term)) total += 3;
    else if (rest.includes(term)) total += 1;
    else if (fixes[index].some((word) => label.includes(word))) total += 2;
    else if (fixes[index].some((word) => rest.includes(word))) total += 0.5;
    else return 0;
  }
  return total + KIND_BOOST[text.entry.kind];
}

/** Edits (insert, delete, substitute, swap two neighbours) from `a` to `b`,
 * or `budget + 1` once it is certain to exceed `budget`. */
export function editDistance(a: string, b: string, budget: number): number {
  let before: number[] = [];
  let previous = Array.from({ length: b.length + 1 }, (_, j) => j);
  for (let i = 1; i <= a.length; i++) {
    const current = [i];
    let rowMin = i;
    for (let j = 1; j <= b.length; j++) {
      const cost = a[i - 1] === b[j - 1] ? 0 : 1;
      let value = Math.min(previous[j] + 1, current[j - 1] + 1, previous[j - 1] + cost);
      if (i > 1 && j > 1 && a[i - 1] === b[j - 2] && a[i - 2] === b[j - 1]) {
        value = Math.min(value, before[j - 2] + 1);
      }
      current.push(value);
      rowMin = Math.min(rowMin, value);
    }
    if (rowMin > budget) return budget + 1;
    before = previous;
    previous = current;
  }
  return previous[b.length];
}

/** Words a request carries that name nothing ("stop the backup"). */
const FILLER: ReadonlySet<string> = new Set(["the", "a", "an", "my", "please", "to", "of", "for"]);

/** The best `limit` entries for `query`; an empty query lists workspaces
 * and incidents first. */
export function rank(entries: readonly PaletteEntry[], query: string, limit = 40): PaletteEntry[] {
  const terms = query
    .toLowerCase()
    .split(/\s+/)
    .filter((term) => term !== "" && !FILLER.has(term));
  if (terms.length === 0) {
    return entries.filter((entry) => entry.kind === "workspace" || entry.kind === "incident").slice(0, limit);
  }
  const near = terms.map(nearTo);
  const asked = askedCommands(terms, near);
  const texts: Text[] = [];
  for (const entry of entries) {
    const command = entry.kind === "command" ? entry.place?.change?.kind : undefined;
    if (command !== undefined && !asked.has(command)) continue;
    texts.push({
      entry,
      label: entry.label.toLowerCase(),
      rest: `${entry.detail.toLowerCase()} ${entry.keywords.toLowerCase()}`,
    });
  }
  const fixes = corrections(entries, texts, terms, near);
  const scored: Array<{ entry: PaletteEntry; value: number }> = [];
  for (const text of texts) {
    const value = score(text, terms, fixes);
    if (value > 0) scored.push({ entry: text.entry, value });
  }
  scored.sort((a, b) => b.value - a.value || a.entry.label.localeCompare(b.entry.label));
  return scored.slice(0, limit).map((item) => item.entry);
}
