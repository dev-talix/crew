import type { RoomMember, RoomMessage } from "./api";

/** Adds a room message, or replaces it when it was already there. */
export function upsertRoomMessage(list: readonly RoomMessage[], message: RoomMessage): RoomMessage[] {
  const at = list.findIndex((m) => m.id === message.id);
  if (at === -1) return [...list, message].sort((a, b) => a.id - b.id);
  const next = list.slice();
  next[at] = message;
  return next;
}

/** Appends streamed text to a reply still being written. A finished reply ignores late chunks. */
export function appendRoomChunk(list: readonly RoomMessage[], messageId: number, chunk: string): RoomMessage[] {
  return list.map((m) => (m.id === messageId && m.status === "streaming" ? { ...m, text: m.text + chunk } : m));
}

/** Joins loaded history with messages that arrived live while it loaded, keeping the newer copy of each. */
export function mergeRoomSnapshot(snapshot: readonly RoomMessage[], live: readonly RoomMessage[]): RoomMessage[] {
  const byId = new Map(snapshot.map((m) => [m.id, m]));
  for (const m of live) {
    const old = byId.get(m.id);
    const newer = !old || (old.status === "streaming" && (m.status !== "streaming" || m.text.length >= old.text.length));
    if (newer) byId.set(m.id, m);
  }
  return [...byId.values()].sort((a, b) => a.id - b.id);
}

// Whole code points, so a character outside the Basic Multilingual Plane counts as one.
const wordBefore = (text: string, at: number) => /[\p{L}\p{N}_]$/u.test(text.slice(0, at));
const wordAt = (text: string, at: number) => /^[\p{L}\p{N}_]/u.test(text.slice(at));

export type Segment = { text: string; memberId?: string };

/**
 * Splits text into plain runs and `@Name` mentions of room members, read the way Grimoire reads them
 * when it picks who answers: whole names, any case, the longest name first, never inside a word.
 */
export function mentionSegments(text: string, members: readonly RoomMember[]): Segment[] {
  const byLength = [...members].sort((a, b) => b.name.length - a.name.length);
  const out: Segment[] = [];
  let plain = "";
  let i = 0;
  while (i < text.length) {
    if (text[i] === "@" && !wordBefore(text, i)) {
      const rest = text.slice(i + 1);
      const hit = byLength.find(
        (m) => m.name && rest.slice(0, m.name.length).toLowerCase() === m.name.toLowerCase() && !wordAt(rest, m.name.length),
      );
      if (hit) {
        if (plain) out.push({ text: plain });
        plain = "";
        out.push({ text: text.slice(i, i + 1 + hit.name.length), memberId: hit.agentId });
        i += 1 + hit.name.length;
        continue;
      }
    }
    plain += text[i];
    i += 1;
  }
  if (plain) out.push({ text: plain });
  return out;
}

/** The `@` word being typed just before the caret, if any: where it starts and what's typed after `@`. */
export function mentionQuery(beforeCaret: string): { start: number; query: string } | null {
  const match = /(^|[^\p{L}\p{N}_])@([\p{L}\p{N}_ ]{0,24})$/u.exec(beforeCaret);
  if (!match) return null;
  const query = match[2];
  if (query.startsWith(" ") || query.endsWith("  ")) return null;
  return { start: beforeCaret.length - query.length - 1, query };
}

/** Members whose name starts with what's typed after `@`. */
export function mentionMatches(query: string, members: readonly RoomMember[]): RoomMember[] {
  const q = query.toLowerCase();
  return members.filter((m) => m.name.toLowerCase().startsWith(q));
}

/** Replaces the `@` word before the caret with a full mention. Returns the new text and caret. */
export function completeMention(text: string, caret: number, name: string): { text: string; caret: number } {
  const found = mentionQuery(text.slice(0, caret));
  const start = found ? found.start : caret;
  // An @ right after a word wouldn't read as a mention.
  const insert = `${wordBefore(text, start) ? " " : ""}@${name} `;
  const after = text.slice(caret).replace(/^ /, "");
  return { text: text.slice(0, start) + insert + after, caret: start + insert.length };
}

/** The line above the composer: who has the floor and who's waiting for it. */
export function floorLine(speaking: string | null, queue: readonly string[], nameOf: (id: string) => string): string {
  if (!speaking) return "";
  const next = queue.filter((id) => id !== speaking).map(nameOf);
  const waiting = next.length ? `, then ${next.join(", ")}` : "";
  return `${nameOf(speaking)} is replying${waiting}`;
}
