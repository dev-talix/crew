import { expect, test } from "bun:test";
import { canImport, groupPreview, importedSummary, previewSummary, type ImportItem, type ImportPreview } from "./transfer";

const item = (what: ImportItem["what"], name: string, finalName = name, skipped = false): ImportItem => ({
  what,
  name,
  finalName,
  conflict: finalName !== name,
  skipped,
  notes: [],
});

const backup: ImportPreview = {
  kind: "backup",
  history: 0,
  items: [
    item("spell", "Topic brief", "Topic brief (2)"),
    item("familiar", "Researcher", "Researcher (2)"),
    item("familiar", "Scribe"),
    item("ritual", "Morning", "Morning", true),
    item("settings", "Settings"),
  ],
};

test("groups the preview in a fixed order", () => {
  expect(groupPreview(backup).map((g) => [g.title, g.items.length])).toEqual([["Familiars", 2], ["Spells", 1], ["Rituals", 1], ["Settings", 1]]);
});

test("sums up what an import adds, renames, and leaves out", () => {
  expect(previewSummary(backup)).toBe('Adds 2 familiars and 1 spell. 2 names are taken, so they get "(2)". 1 item can\'t come in.');
  const one: ImportPreview = { kind: "spell", history: 0, items: [item("spell", "Brief", "Brief", true)] };
  expect(previewSummary(one)).toBe("Nothing new to add. 1 item can't come in.");
  expect(canImport(one)).toBe(false);
  expect(canImport(backup)).toBe(true);
});

test("says what was imported", () => {
  expect(importedSummary({ familiars: 2, spells: 1, rituals: 0, covens: 0, themes: 1, settings: true })).toBe("Imported 2 familiars, 1 spell, 1 theme and settings.");
  expect(importedSummary({ familiars: 0, spells: 0, rituals: 0, covens: 1, themes: 0, settings: false })).toBe("Imported 1 coven.");
});
