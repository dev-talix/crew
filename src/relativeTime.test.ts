import { expect, test } from "bun:test";
import { relativeTime } from "./api";

test("rounds to the nearest unit without saying 60 minutes", () => {
  const now = 1_800_000_000_000;
  expect(relativeTime(now + 3_600_000 - 5_000, now)).toBe("in 1 hour");
  expect(relativeTime(now + 31 * 60_000, now)).toBe("in 31 minutes");
  expect(relativeTime(now - 2 * 86_400_000, now)).toBe("2 days ago");
});
