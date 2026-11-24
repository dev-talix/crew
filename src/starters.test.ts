import { expect, test } from "bun:test";
import type { ProviderStatus } from "./api";
import { entriesSize, ENTRY_LIMIT, MEMORY_LIMIT } from "./memory";
import { defaultProvider, ready, starterDraft, STARTERS } from "./starters";

const status = (provider: "codex" | "claude", installed: boolean, signedIn: boolean | null): ProviderStatus => ({
  provider,
  installed,
  version: null,
  signedIn,
  method: null,
  detail: null,
});

test("every starter fits a familiar's memory", () => {
  for (const s of STARTERS) {
    const { memory } = starterDraft(s, "", "codex");
    expect(memory.every((m) => m.text.length <= ENTRY_LIMIT)).toBe(true);
    expect(entriesSize(memory)).toBeLessThanOrEqual(MEMORY_LIMIT);
  }
  expect(new Set(STARTERS.map((s) => s.id)).size).toBe(STARTERS.length);
});

test("a starter gets the typed name and workspace access", () => {
  const draft = starterDraft(STARTERS[0], "  Scout ", "claude");
  expect(draft).toMatchObject({ name: "Scout", provider: "claude", access: "workspace", rituals: [] });
  expect(starterDraft(STARTERS[0], "   ", "codex").name).toBe(STARTERS[0].name);
});

test("picks the first signed-in provider", () => {
  expect(ready(status("codex", true, true))).toBe(true);
  expect(ready(status("codex", true, null))).toBe(false);
  expect(ready(undefined)).toBe(false);
  expect(defaultProvider([status("codex", true, true), status("claude", true, true)])).toBe("codex");
  expect(defaultProvider([status("codex", false, null), status("claude", true, true)])).toBe("claude");
  expect(defaultProvider([status("codex", true, false), status("claude", false, null)])).toBeNull();
});
