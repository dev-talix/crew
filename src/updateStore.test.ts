import { expect, test } from "bun:test";
import { isDue, shouldInstall } from "./updateStore";

test("checks once a day", () => {
  const now = 1_800_000_000_000;
  expect(isDue(null, now)).toBe(true);
  expect(isDue(Number.NaN, now)).toBe(true);
  expect(isDue(now - 3_600_000, now)).toBe(false);
  expect(isDue(now - 86_400_000, now)).toBe(true);
});

test("installs on its own only when automatic updates are on and the build is signed", () => {
  const check = { current: "0.4.0", latest: { version: "0.5.0", url: "", publishedAt: null }, newer: true, canInstall: true };
  expect(shouldInstall(check, true)).toBe(true);
  expect(shouldInstall(check, false)).toBe(false);
  expect(shouldInstall({ ...check, canInstall: false }, true)).toBe(false);
  expect(shouldInstall({ ...check, newer: false }, true)).toBe(false);
  expect(shouldInstall(null, true)).toBe(false);
});
