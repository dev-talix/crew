import { relativeTime } from "./api";

/** A familiar's check-in schedule and memory review switch. */
export interface CheckInSettings {
  enabled: boolean;
  everyMinutes: number;
  /** "HH:MM". An end before the start runs past midnight; the same time means all day. */
  start: string;
  end: string;
  /** 1 (Monday) to 7 (Sunday). Empty means every day. */
  days: number[];
  reviewChats: boolean;
}

export interface CheckInStatus extends CheckInSettings {
  nextRunAt: number | null;
  lastRunAt: number | null;
  lastNote: string | null;
}

/** A check-in that asked, drafted, said something, or didn't finish. Quiet ones leave none. */
export interface CheckInRun {
  id: string;
  agentId: string;
  /** Its hidden chat, with the transcript. */
  chatId: string;
  status: "running" | "done" | "failed" | "timeout" | "stopped" | "interrupted";
  summary: string | null;
  startedAt: number;
  finishedAt: number | null;
}

export const MIN_EVERY = 15;

export const settingsOf = (status: CheckInStatus): CheckInSettings => ({
  enabled: status.enabled,
  everyMinutes: status.everyMinutes,
  start: status.start,
  end: status.end,
  days: status.days,
  reviewChats: status.reviewChats,
});

export const sameSettings = (a: CheckInSettings, b: CheckInSettings) => JSON.stringify(a) === JSON.stringify(b);

/** The line under the switch: when the next one runs and how the last one went. */
export function checkInNote(status: CheckInStatus, now = Date.now()): string {
  const parts: string[] = [];
  if (status.enabled && status.nextRunAt) parts.push(`Next check-in ${relativeTime(status.nextRunAt, now)}.`);
  if (status.lastRunAt && status.lastNote) parts.push(`Last one ${relativeTime(status.lastRunAt, now)}: ${status.lastNote}`);
  else if (status.lastNote) parts.push(status.lastNote);
  return parts.join(" ");
}
