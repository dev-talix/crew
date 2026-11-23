import type { Message } from "./api";

/** Adds a side conversation's message, or replaces it when it was already there. */
export function upsert(messages: readonly Message[], message: Message): Message[] {
  const at = messages.findIndex((m) => m.id === message.id);
  if (at === -1) return [...messages, message];
  const next = messages.slice();
  next[at] = message;
  return next;
}

/** Appends streamed text to a reply that is still being written. */
export function appendChunk(messages: readonly Message[], messageId: number, chunk: string): Message[] {
  return messages.map((m) => (m.id === messageId ? { ...m, text: m.text + chunk } : m));
}

/** A finished message beats one still streaming; otherwise the longer text is the later one. */
const isNewer = (a: Message, b: Message) =>
  (a.status === "streaming") !== (b.status === "streaming") ? a.status !== "streaming" : a.text.length >= b.text.length;

/** Joins loaded history with messages that arrived live while it loaded, keeping the newer of each. */
export function mergeSnapshot(snapshot: readonly Message[], live: readonly Message[]): Message[] {
  const byId = new Map(snapshot.map((m) => [m.id, m]));
  for (const m of live) {
    const old = byId.get(m.id);
    if (!old || isNewer(m, old)) byId.set(m.id, m);
  }
  return [...byId.values()].sort((a, b) => a.id - b.id);
}

export type SideState = "working" | "replied" | "failed" | "empty";

/** Where a side conversation stands, from its latest message. */
export function sideState(messages: readonly Message[]): SideState {
  const last = messages[messages.length - 1];
  if (!last) return "empty";
  if (last.role === "error") return "failed";
  if (last.role === "assistant" && last.status !== "streaming") return "replied";
  return "working";
}

/** The other familiar's name, from a link row like "Asked Scribe" or "Messaged Scribe". */
export function partnerName(label: string): string {
  return label.replace(/^(Asked|Messaged) /, "");
}
