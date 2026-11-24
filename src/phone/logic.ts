import type { Draft, InboxItem, InboxOption, Message } from "../api";
import { isApproval, optionsInOrder } from "../inboxList";

export type PhoneEvent =
  | { kind: "message"; payload: Message }
  | { kind: "delta"; payload: { agentId: string; messageId: number; chunk: string } }
  | { kind: "status"; payload: { agentId: string; busy: boolean } }
  | { kind: "chat"; payload: { agentId: string } }
  | { kind: "inbox" | "drafts" | "familiars"; payload?: unknown };

/**
 * One familiar's chat after an event. A message event adds or replaces a message by id; a delta
 * appends its chunk to the message it names. Events for other familiars leave the list alone.
 */
export function applyChatEvent(messages: Message[], agentId: string, event: PhoneEvent): Message[] {
  if (event.kind === "message") {
    const message = event.payload;
    if (message.agentId !== agentId) return messages;
    const at = messages.findIndex((m) => m.id === message.id);
    if (at === -1) return [...messages, message].sort((a, b) => a.id - b.id);
    return messages.map((m, i) => (i === at ? message : m));
  }
  if (event.kind === "delta") {
    const { messageId, chunk } = event.payload;
    if (event.payload.agentId !== agentId) return messages;
    return messages.map((m) => (m.id === messageId ? { ...m, text: m.text + chunk, status: "streaming" } : m));
  }
  return messages;
}

/**
 * A chat's history merged with what arrived while it loaded. A message the events added after the
 * history was read, or a longer copy of a reply still streaming, wins over the history's copy. A
 * finished reply in the history beats a partial one.
 */
export function mergeHistory(history: Message[], live: Message[]): Message[] {
  const byId = new Map(history.map((m) => [m.id, m]));
  for (const m of live) {
    const old = byId.get(m.id);
    const longer = old?.status === "streaming" && m.status === "streaming" && m.text.length > old.text.length;
    const newer = !old || longer || (m.updatedAt ?? m.createdAt) > (old.updatedAt ?? old.createdAt);
    if (newer) byId.set(m.id, m);
  }
  return [...byId.values()].sort((a, b) => a.id - b.id);
}

/** The answers a phone may give. Always allow is a lasting grant, so approvals leave it to the Mac. */
export function phoneOptions(item: InboxItem): InboxOption[] {
  const options = optionsInOrder(item);
  return isApproval(item) ? options.filter((o) => o.value !== "always") : options;
}

/** Approvals first, since they time out, then questions, oldest first. */
export function sortInbox(items: InboxItem[]): InboxItem[] {
  return [...items].sort((a, b) => Number(isApproval(b)) - Number(isApproval(a)) || (a.createdAt ?? 0) - (b.createdAt ?? 0));
}

export function titleText(waiting: number): string {
  return waiting > 0 ? `(${waiting}) Grimoire` : "Grimoire";
}

export const pendingDrafts = (drafts: Draft[]) => drafts.filter((d) => d.state === "draft");

type NameOf = (agentId: string) => string;

/** The banner for inbox items that weren't there before, or null. `before` is null on the first load. */
export function inboxAlert(before: Set<string> | null, items: InboxItem[], nameOf: NameOf): string | null {
  if (!before) return null;
  const fresh = sortInbox(items.filter((i) => !before.has(i.id)));
  if (fresh.length === 0) return null;
  const first = fresh[0];
  const head = isApproval(first) ? `${nameOf(first.agentId)} needs your OK` : `${nameOf(first.agentId)} asks: ${first.title}`;
  return fresh.length > 1 ? `${head} (and ${fresh.length - 1} more)` : head;
}

/** The banner for pending drafts that weren't there before, or null. */
export function draftAlert(before: Set<string> | null, drafts: Draft[], nameOf: NameOf): string | null {
  if (!before) return null;
  const fresh = pendingDrafts(drafts).filter((d) => !before.has(d.id));
  if (fresh.length === 0) return null;
  return fresh.length > 1 ? `${fresh.length} new drafts to send` : `${nameOf(fresh[0].agentId)} wrote a draft`;
}

/** The banner for a finished reply, unless the person is looking at that chat. */
export function replyAlert(event: PhoneEvent, watching: string | null, nameOf: NameOf): string | null {
  if (event.kind !== "status" || event.payload.busy) return null;
  if (event.payload.agentId === watching) return null;
  return `${nameOf(event.payload.agentId)} replied`;
}

/** How long to wait before retry number `attempt` (0-based): 2s, doubling, at most 30s. */
export function backoff(attempt: number): number {
  return Math.min(30_000, 2_000 * 2 ** Math.max(0, attempt));
}

/**
 * "5 minutes ago". The desktop's relativeTime lives in src/api.ts, which imports Tauri, so the phone
 * keeps its own short copy.
 */
export function timeAgo(ms: number, now = Date.now()): string {
  const diff = ms - now;
  const format = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
  for (const [unit, size] of [
    ["day", 86_400_000],
    ["hour", 3_600_000],
    ["minute", 60_000],
  ] as const) {
    if (Math.abs(diff) >= size * 0.98) return format.format(Math.round(diff / size), unit);
  }
  return "just now";
}
