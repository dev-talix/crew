import { relativeTime, type Agent } from "./api";

/** The backend's limits, used until a memory has loaded. */
export const MEMORY_LIMIT = 16 * 1024;
export const ENTRY_LIMIT = 1000;

export type MemoryScope = "familiar" | "coven";

/** fact: lasting, about the person or their world. rule: a standing instruction. project: a note about one project. */
export type MemoryKind = "fact" | "rule" | "project";

/** The editor's sections, rules first, as the familiar reads them. */
export const MEMORY_KINDS: { kind: MemoryKind; title: string; empty: string }[] = [
  { kind: "rule", title: "Rules", empty: "No rules yet. Make one from a message in the chat, or add one here." },
  { kind: "fact", title: "Facts", empty: "No facts yet." },
  { kind: "project", title: "Projects", empty: "No project notes yet." },
];

export interface MemoryEntry {
  id: string;
  kind: MemoryKind;
  /** Set for project entries only. */
  project?: string;
  text: string;
  /** YYYY-MM-DD, the day Grimoire saved it. */
  learnedOn: string;
  /** The familiar that saved it; null when the user did. */
  source: string | null;
}

export interface Memory {
  entries: MemoryEntry[];
  /** Bytes of UTF-8 a memory may hold across its entries. */
  limit: number;
  /** Characters one entry may hold. */
  entryLimit: number;
  /** The familiar that last changed it; null when the user did, or nobody has yet. */
  updatedBy: string | null;
  updatedAt: number | null;
}

/** An entry as the editor sends it: an id for one that exists, none for a new one. */
export interface MemoryDraft {
  id?: string;
  kind: MemoryKind;
  project?: string;
  text: string;
}

const kb = (bytes: number) => `${(bytes / 1024).toFixed(1).replace(/\.0$/, "")} KB`;

/** Bytes the backend counts for these entries: their trimmed text and project names in UTF-8. */
export function entriesSize(entries: Pick<MemoryDraft, "kind" | "project" | "text">[]): number {
  const encoder = new TextEncoder();
  return entries.reduce((sum, e) => sum + encoder.encode(e.text.trim()).length + (e.kind === "project" ? encoder.encode((e.project ?? "").trim()).length : 0), 0);
}

/** How much of the limit `bytes` uses. */
export function memoryUsage(bytes: number, limit: number): { label: string; over: boolean } {
  const over = bytes > limit;
  return { label: `${kb(bytes)} of ${kb(limit)}${over ? ", over the limit" : ""}`, over };
}

/** Why the backend would refuse this entry, or null when it's fine. */
export function entryProblem(entry: Pick<MemoryDraft, "kind" | "project" | "text">, limit = ENTRY_LIMIT): string | null {
  const length = [...entry.text.trim()].length;
  if (!length) return "Write something or delete it.";
  if (length > limit) return `Up to ${limit} characters; this is ${length}.`;
  if (entry.kind === "project" && !(entry.project ?? "").trim()) return "Name the project.";
  return null;
}

/** A chat message as a rule: trimmed and cut to the entry limit. */
export function ruleText(message: string, limit = ENTRY_LIMIT): string {
  const chars = [...message.trim()];
  return chars.length > limit ? chars.slice(0, limit).join("").trimEnd() : chars.join("");
}

/** "Learned Nov 3, 2026 by Scribe", or "Added ... by you" for the user's own. */
export function entryLabel(entry: Pick<MemoryEntry, "learnedOn" | "source">, agents: Agent[]): string {
  const [year, month, day] = entry.learnedOn.split("-").map(Number);
  const date = new Date(year, month - 1, day).toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" });
  if (entry.source === null) return `Added ${date} by you`;
  return `Learned ${date} by ${agents.find((a) => a.id === entry.source)?.name ?? "a deleted familiar"}`;
}

/** "Last updated by Scribe 2 hours ago", or "you" when the user saved it. */
export function lastUpdated(memory: Pick<Memory, "updatedBy" | "updatedAt">, agents: Agent[], now = Date.now()): string {
  if (memory.updatedAt === null) return "Nothing saved yet";
  const who = memory.updatedBy === null ? "you" : (agents.find((a) => a.id === memory.updatedBy)?.name ?? "a deleted familiar");
  return `Last updated by ${who} ${relativeTime(memory.updatedAt, now)}`;
}
