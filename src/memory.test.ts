import { expect, test } from "bun:test";
import type { Agent } from "./api";
import { lastUpdated, memoryUsage } from "./memory";

test("counts UTF-8 bytes against the limit", () => {
  expect(memoryUsage("", 16384)).toEqual({ label: "0 KB of 16 KB", over: false });
  expect(memoryUsage("é".repeat(1024), 16384).label).toBe("2 KB of 16 KB");
  expect(memoryUsage("x".repeat(16385), 16384)).toEqual({ label: "16 KB of 16 KB, over the limit", over: true });
});

test("names who last updated a memory", () => {
  const now = 1_800_000_000_000;
  const agents = [{ id: "s1", name: "Scribe" }] as Agent[];
  expect(lastUpdated({ updatedBy: null, updatedAt: null }, agents, now)).toBe("Nothing saved yet");
  expect(lastUpdated({ updatedBy: "s1", updatedAt: now - 2 * 3_600_000 }, agents, now)).toBe("Last updated by Scribe 2 hours ago");
  expect(lastUpdated({ updatedBy: null, updatedAt: now - 30_000 }, agents, now)).toBe("Last updated by you just now");
  expect(lastUpdated({ updatedBy: "gone", updatedAt: now - 86_400_000 }, agents, now)).toBe("Last updated by a deleted familiar yesterday");
});
