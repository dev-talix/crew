import { expect, test } from "bun:test";
import { checkInNote, sameSettings, settingsOf, type CheckInStatus } from "./checkins";

const now = 1_800_000_000_000;
const status: CheckInStatus = {
  enabled: true,
  everyMinutes: 30,
  start: "09:00",
  end: "17:00",
  days: [1, 2, 3, 4, 5],
  reviewChats: false,
  nextRunAt: now + 30 * 60_000,
  lastRunAt: now - 60 * 60_000,
  lastNote: "Nothing needed you.",
};

test("says when the next check-in runs and how the last went", () => {
  expect(checkInNote(status, now)).toBe("Next check-in in 30 minutes. Last one 1 hour ago: Nothing needed you.");
  expect(checkInNote({ ...status, enabled: false, nextRunAt: null }, now)).toBe("Last one 1 hour ago: Nothing needed you.");
  expect(checkInNote({ ...status, lastRunAt: null, lastNote: null }, now)).toBe("Next check-in in 30 minutes.");
  // A skip has a note but no run time.
  expect(checkInNote({ ...status, nextRunAt: null, lastRunAt: null, lastNote: "Skipped: Ada was in the middle of a turn." }, now)).toBe(
    "Skipped: Ada was in the middle of a turn.",
  );
});

test("compares only the settings the person edits", () => {
  const a = settingsOf(status);
  expect(sameSettings(a, settingsOf({ ...status, lastNote: "other" }))).toBe(true);
  expect(sameSettings(a, { ...a, days: [1] })).toBe(false);
});
