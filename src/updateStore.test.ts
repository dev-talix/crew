import { expect, test } from "bun:test";
import { isDue } from "./updateStore";

test("checks once a day", () => {
  const now = 1_800_000_000_000;
  expect(isDue(null, now)).toBe(true);
  expect(isDue(Number.NaN, now)).toBe(true);
  expect(isDue(now - 3_600_000, now)).toBe(false);
  expect(isDue(now - 86_400_000, now)).toBe(true);
});
