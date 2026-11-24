import { describe, expect, test } from "bun:test";
import { draftFromTemplate, guessSchedule, matchesSearch, memoryFit, permissionLines, splitText, type MarketTemplate } from "./templates";

const haggle: MarketTemplate = {
  slug: "haggle-bot",
  name: "Haggle Bot",
  creator: "Daniel Gartshein",
  description: "Finds SaaS savings.",
  categories: ["From Grok Bot Team"],
  instructions: "",
  memories: [{ name: "memory 1", description: "Never spend money." }, { name: "memory 2", description: "  " }],
  skills: [{ name: "haggle-setup", description: "First-run interview." }],
  routines: [
    { name: "Renewal window watch", description: "Disabled by default. Weekly scan of renewals." },
    { name: "Monthly inventory refresh", description: "Re-pull Ramp cards." },
  ],
  integrations: [{ name: "Ramp", description: "" }, { name: "Gmail", description: "" }],
  sourceUrl: "https://x.ai/bot/marketplace/bots/haggle-bot",
  sharePath: "/bot/pwQ612YrX3R0eACnIMlom",
};

describe("draftFromTemplate", () => {
  test("maps description, memories, and skills to rules and integrations to one fact", () => {
    const draft = draftFromTemplate(haggle);
    expect(draft.name).toBe("Haggle Bot");
    expect(draft.memory.map((r) => [r.origin, r.kind])).toEqual([
      ["description", "rule"],
      ["memory", "rule"],
      ["skill", "rule"],
      ["integrations", "fact"],
    ]);
    expect(draft.memory[0].text).toBe("You are Haggle Bot. Finds SaaS savings.");
    expect(draft.memory[2].text).toBe("When asked to haggle-setup: First-run interview.");
    expect(draft.memory[3].text).toContain("Ramp, Gmail");
  });

  test("turns routines into rituals with guessed schedules", () => {
    const [watch, refresh] = draftFromTemplate(haggle).rituals;
    expect(watch.schedule).toEqual({ kind: "daily", time: "09:00", days: [1] });
    expect(refresh.schedule).toEqual({ kind: "interval", minutes: 10080 });
    expect(refresh.scheduleNote).toContain("weekly");
    expect(watch.prompt).toBe("Weekly scan of renewals.");
  });

  test("splits long memories and leaves rows past the limit unchecked", () => {
    const long = Array.from({ length: 40 }, (_, i) => `Sentence number ${i} is here to fill space.`).join(" ");
    const many = { ...haggle, memories: Array.from({ length: 30 }, () => ({ name: "m", description: long })) };
    const draft = draftFromTemplate(many);
    expect(draft.memory.every((r) => r.text.length <= 1000)).toBe(true);
    expect(draft.memory.some((r) => !r.include)).toBe(true);
    expect(memoryFit(draft.memory).fits).toBe(true);
    expect(memoryFit(draft.memory.map((r) => ({ ...r, include: true }))).fits).toBe(false);
  });
});

test("splitText cuts at sentence ends", () => {
  const text = `${"a".repeat(600)}. ${"b".repeat(600)}.`;
  expect(splitText(text)).toEqual([`${"a".repeat(600)}.`, `${"b".repeat(600)}.`]);
  expect(splitText("x".repeat(2500)).map((p) => p.length)).toEqual([1000, 1000, 500]);
});

test("guessSchedule reads common words and falls back to Mondays", () => {
  expect(guessSchedule("Daily digest").schedule).toEqual({ kind: "daily", time: "09:00", days: [] });
  expect(guessSchedule("every hour check").schedule).toEqual({ kind: "interval", minutes: 60 });
  expect(guessSchedule("on weekdays").schedule).toEqual({ kind: "daily", time: "09:00", days: [1, 2, 3, 4, 5] });
  expect(guessSchedule("whenever").note).toContain("no schedule");
});

test("permissionLines states access and what isn't connected", () => {
  const lines = permissionLines(haggle, "workspace", "codex");
  expect(lines[0]).toContain("sandbox");
  expect(permissionLines(haggle, "workspace", "claude")[0]).toContain("Permissions settings");
  expect(lines.join(" ")).toContain("won't connect: Ramp, Gmail");
  expect(permissionLines(haggle, "full", "codex")[0]).toContain("Full access: on");
});

test("permissionLines leaves apps a connector covers out of the won't-connect line", () => {
  const lines = permissionLines(haggle, "workspace", "codex", ["Ramp"]).join(" ");
  expect(lines).toContain("Connectors below can link Ramp once you sign in.");
  expect(lines).toContain("won't connect: Gmail.");
  expect(lines).not.toContain("connectors are installed");
});

test("matchesSearch needs every word", () => {
  expect(matchesSearch(haggle, "saas daniel")).toBe(true);
  expect(matchesSearch(haggle, "saas figma")).toBe(false);
  expect(matchesSearch(haggle, "")).toBe(true);
});
