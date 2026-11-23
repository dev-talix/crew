import type { Handoff, HandoffStatus } from "./api";

export const STATUS_LABELS: Record<HandoffStatus, string> = {
  waiting: "Waiting",
  accepted: "Accepted",
  done: "Done",
  failed: "Failed",
};

/** The text of the link row in the sender's chat, which SideConversation reads the partner from. */
export const linkLabel = (handoff: Handoff, toName: string) => `${handoff.kind === "ask" ? "Asked" : "Messaged"} ${toName}`;

/** Streamed reply text means the other familiar took a waiting handoff on. */
export function startedReplying(list: readonly Handoff[], chatId: string, now = Date.now()): Handoff[] {
  return list.map((h) => (h.id === chatId && h.status === "waiting" ? { ...h, status: "accepted", updatedAt: now } : h));
}
