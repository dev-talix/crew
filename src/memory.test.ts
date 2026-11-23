import { expect, test } from "bun:test";
import type { Agent } from "./api";
import { entriesSize, entryLabel, entryProblem, lastUpdated, memoryChanges, memoryUsage, ruleText, type MemoryEntry } from "./memory";

test("a save sends only changed entries and the person's deletions", () => {
  const entry = (id: string, text: string, project?: string): MemoryEntry => ({ id, kind: project ? "project" : "fact", project, text, learnedOn: "2026-11-01", source: null });
  const kept = entry("kept", "Likes tea.");
  const edited = entry("edited", "Uses npm.");
  const moved = entry("moved", "Ships Fridays.", "app");
  const gone = entry("gone", "Out of date.");
  // The editor opened on these four; a refresh then brought in "added-by-familiar", which has no row.
  const rows = [
    { saved: kept, kind: kept.kind, project: "", text: kept.text },
    { saved: edited, kind: edited.kind, project: "", text: "Uses bun." },
    { saved: moved, kind: moved.kind, project: "web", text: moved.text },
    { kind: "rule" as const, project: "ignored", text: "Be brief." },
  ];
  expect(memoryChanges(rows, [gone.id])).toEqual({
    entries: [
      { id: "edited", kind: "fact", project: undefined, text: "Uses bun." },
      { id: "moved", kind: "project", project: "web", text: "Ships Fridays." },
      { id: undefined, kind: "rule", project: undefined, text: "Be brief." },
    ],
    removed: ["gone"],
  });
  expect(memoryChanges([{ saved: moved, kind: "project", project: "app", text: moved.text }], [])).toEqual({ entries: [], removed: [] });
});

test("counts UTF-8 bytes of entries against the limit", () => {
  expect(memoryUsage(0, 16384)).toEqual({ label: "0 KB of 16 KB", over: false });
  expect(memoryUsage(entriesSize([{ kind: "fact", text: " " + "é".repeat(1024) + " " }]), 16384).label).toBe("2 KB of 16 KB");
  expect(entriesSize([{ kind: "project", project: " app ", text: "ab" }, { kind: "rule", project: "ignored", text: "c" }])).toBe(6);
  expect(memoryUsage(16385, 16384)).toEqual({ label: "16 KB of 16 KB, over the limit", over: true });
});

test("checks entries like the backend", () => {
  expect(entryProblem({ kind: "fact", text: "  " })).toBe("Write something or delete it.");
  expect(entryProblem({ kind: "project", project: " ", text: "Uses bun." })).toBe("Name the project.");
  expect(entryProblem({ kind: "project", project: "app", text: "Uses bun." })).toBeNull();
  expect(entryProblem({ kind: "rule", text: "é".repeat(1001) })).toBe("Up to 1000 characters; this is 1001.");
  expect(entryProblem({ kind: "rule", text: "é".repeat(1000) })).toBeNull();
});

test("turns a message into a bounded rule", () => {
  expect(ruleText("  Use metric units from now on.\n")).toBe("Use metric units from now on.");
  expect([...ruleText("😀".repeat(1200))].length).toBe(1000);
  expect(ruleText("abcd efgh", 5)).toBe("abcd");
});

test("labels when and by whom an entry was learned", () => {
  const agents = [{ id: "s1", name: "Scribe" }] as Agent[];
  const date = new Date(2026, 10, 3).toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" });
  expect(entryLabel({ learnedOn: "2026-11-03", source: "s1" }, agents)).toBe(`Learned ${date} by Scribe`);
  expect(entryLabel({ learnedOn: "2026-11-03", source: null }, agents)).toBe(`Added ${date} by you`);
  expect(entryLabel({ learnedOn: "2026-11-03", source: "gone" }, agents)).toBe(`Learned ${date} by a deleted familiar`);
});

test("names who last updated a memory", () => {
  const now = 1_800_000_000_000;
  const agents = [{ id: "s1", name: "Scribe" }] as Agent[];
  expect(lastUpdated({ updatedBy: null, updatedAt: null }, agents, now)).toBe("Nothing saved yet");
  expect(lastUpdated({ updatedBy: "s1", updatedAt: now - 2 * 3_600_000 }, agents, now)).toBe("Last updated by Scribe 2 hours ago");
  expect(lastUpdated({ updatedBy: null, updatedAt: now - 30_000 }, agents, now)).toBe("Last updated by you just now");
  expect(lastUpdated({ updatedBy: "gone", updatedAt: now - 86_400_000 }, agents, now)).toBe("Last updated by a deleted familiar yesterday");
});
