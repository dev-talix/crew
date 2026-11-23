// Export and import: the types the backend sends, and how the import preview groups them.

export type ExportKind = "familiar" | "spell" | "coven" | "theme";

export type ItemKind = "familiar" | "spell" | "ritual" | "coven" | "theme" | "settings";

export interface ImportItem {
  what: ItemKind;
  /** The name in the file. */
  name: string;
  /** The name it gets here, with "(2)" when the name is taken. */
  finalName: string;
  conflict: boolean;
  /** It can't be imported; the notes say why. */
  skipped: boolean;
  notes: string[];
}

export interface ImportPreview {
  kind: ExportKind | "backup";
  items: ImportItem[];
  /** Chat messages that come with the familiars. */
  history: number;
  /** A familiar asks for full access. It comes in with workspace access unless the person keeps it. */
  fullAccess: boolean;
}

export interface Picked {
  file: string;
  preview: ImportPreview;
}

export interface Imported {
  familiars: number;
  spells: number;
  rituals: number;
  covens: number;
  themes: number;
  settings: boolean;
}

const ORDER: { what: ItemKind; one: string; many: string }[] = [
  { what: "familiar", one: "familiar", many: "Familiars" },
  { what: "spell", one: "spell", many: "Spells" },
  { what: "ritual", one: "ritual", many: "Rituals" },
  { what: "coven", one: "coven", many: "Covens" },
  { what: "theme", one: "theme", many: "Themes" },
  { what: "settings", one: "settings", many: "Settings" },
];

const plural = (n: number, one: string) => `${n} ${one}${n === 1 || one === "settings" ? "" : "s"}`;

/** The preview's items in sections, in a fixed order, leaving out empty ones. */
export function groupPreview(preview: ImportPreview) {
  return ORDER.map((o) => ({ what: o.what, title: o.many, items: preview.items.filter((i) => i.what === o.what) })).filter((g) => g.items.length > 0);
}

/** "Adds 2 familiars and 1 spell. 1 name is taken, so it gets a new one." */
export function previewSummary(preview: ImportPreview): string {
  const adding = preview.items.filter((i) => !i.skipped && i.what !== "settings");
  const counts = ORDER.filter((o) => o.what !== "settings")
    .map((o) => [o.one, adding.filter((i) => i.what === o.what).length] as const)
    .filter(([, n]) => n > 0)
    .map(([one, n]) => plural(n, one));
  const parts: string[] = [];
  if (counts.length) parts.push(`Adds ${counts.length > 1 ? `${counts.slice(0, -1).join(", ")} and ${counts[counts.length - 1]}` : counts[0]}.`);
  else parts.push("Nothing new to add.");
  const conflicts = preview.items.filter((i) => i.conflict && !i.skipped).length;
  if (conflicts) parts.push(`${conflicts === 1 ? "1 name is" : `${conflicts} names are`} taken, so ${conflicts === 1 ? "it gets" : "they get"} "(2)".`);
  const skipped = preview.items.filter((i) => i.skipped).length;
  if (skipped) parts.push(`${plural(skipped, "item")} can't come in.`);
  return parts.join(" ");
}

/** Whether applying would change anything. */
export function canImport(preview: ImportPreview): boolean {
  return preview.items.some((i) => !i.skipped);
}

/** "Imported 2 familiars, 1 spell, and settings." */
export function importedSummary(done: Imported): string {
  const parts = ORDER.filter((o) => o.what !== "settings")
    .map((o) => {
      const n = done[`${o.what}s` as keyof Imported] as number;
      return n ? plural(n, o.one) : "";
    })
    .filter(Boolean);
  if (done.settings) parts.push("settings");
  if (!parts.length) return "Nothing was imported.";
  const list = parts.length > 1 ? `${parts.slice(0, -1).join(", ")} and ${parts[parts.length - 1]}` : parts[0];
  return `Imported ${list}.`;
}
