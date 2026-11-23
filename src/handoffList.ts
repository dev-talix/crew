import type { Handoff, HandoffStatus } from "./api";

export const STATUS_LABELS: Record<HandoffStatus, string> = {
  waiting: "Waiting",
  accepted: "Accepted",
  done: "Done",
  failed: "Failed",
};

/** The text of the link row in the sender's chat, which SideConversation reads the partner from. */
export const linkLabel = (handoff: Handoff, toName: string) => `${handoff.kind === "ask" ? "Asked" : "Messaged"} ${toName}`;

/** A familiar as the list names it: "Scribe", or "Scribe on NigelBot" when it lives on a linked Mac. */
export const familiarLabel = (name: string, mac: string | null) => (mac ? `${name} on ${mac}` : name);
