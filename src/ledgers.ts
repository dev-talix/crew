import { relativeTime } from "./api";

export const LEDGER_STATUSES = ["idle", "working", "waiting", "blocked", "done"] as const;
export type LedgerStatus = (typeof LEDGER_STATUSES)[number];

/** A familiar's goal, status, and what it's waiting on, kept across restarts. */
export interface Ledger {
  goal: string;
  status: LedgerStatus;
  waitingOn: string;
  nextStep: string;
  /** Null until someone saves it. */
  updatedAt: number | null;
  updatedBy: "familiar" | "you" | null;
}

export const STATUS_NAMES: Record<LedgerStatus, string> = {
  idle: "Idle",
  working: "Working",
  waiting: "Waiting",
  blocked: "Blocked",
  done: "Done",
};

export function ledgerIsBlank(ledger: Ledger): boolean {
  return !ledger.goal && ledger.status === "idle" && !ledger.waitingOn && !ledger.nextStep;
}

/** "Updated by Scribe 5 minutes ago", or "by you" when the person saved it. */
export function ledgerUpdated(ledger: Pick<Ledger, "updatedAt" | "updatedBy">, familiar: string, now = Date.now()): string {
  if (ledger.updatedAt === null) return "";
  return `Updated by ${ledger.updatedBy === "you" ? "you" : familiar} ${relativeTime(ledger.updatedAt, now)}`;
}
