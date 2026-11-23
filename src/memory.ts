import { relativeTime, type Agent } from "./api";

/** The backend's limit, used until a memory has loaded. */
export const MEMORY_LIMIT = 16 * 1024;

export type MemoryScope = "familiar" | "coven";

export interface Memory {
  text: string;
  /** Bytes of UTF-8 a memory may hold. */
  limit: number;
  /** The familiar that last changed it; null when the user did, or nobody has yet. */
  updatedBy: string | null;
  updatedAt: number | null;
}

const kb = (bytes: number) => `${(bytes / 1024).toFixed(1).replace(/\.0$/, "")} KB`;

/** How much of the limit `text` uses, counted in UTF-8 bytes like the backend. */
export function memoryUsage(text: string, limit: number): { label: string; over: boolean } {
  const bytes = new TextEncoder().encode(text).length;
  const over = bytes > limit;
  return { label: `${kb(bytes)} of ${kb(limit)}${over ? ", over the limit" : ""}`, over };
}

/** "Last updated by Scribe 2 hours ago", or "you" when the user saved it. */
export function lastUpdated(memory: Pick<Memory, "updatedBy" | "updatedAt">, agents: Agent[], now = Date.now()): string {
  if (memory.updatedAt === null) return "Nothing saved yet";
  const who = memory.updatedBy === null ? "you" : (agents.find((a) => a.id === memory.updatedBy)?.name ?? "a deleted familiar");
  return `Last updated by ${who} ${relativeTime(memory.updatedAt, now)}`;
}
