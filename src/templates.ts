import type { Access, Provider, Schedule } from "./api";
import type { Avatar } from "./avatar";
import { ENTRY_LIMIT, MEMORY_LIMIT, entriesSize, type MemoryKind } from "./memory";

/** A titled line from a marketplace template: a memory, skill, routine, or integration. */
export interface TemplateItem {
  name: string;
  description: string;
}

/** A Grok Bot marketplace template, as the backend reads it from x.ai. All of it is untrusted text. */
export interface MarketTemplate {
  slug: string;
  name: string;
  creator: string;
  description: string;
  categories: string[];
  instructions: string;
  memories: TemplateItem[];
  skills: TemplateItem[];
  routines: TemplateItem[];
  integrations: TemplateItem[];
  sourceUrl: string;
  sharePath: string;
}

/** Where a memory row came from in the template, shown beside it in the preview. */
export type Origin = "description" | "instructions" | "memory" | "skill" | "integrations";

export interface MemoryRow {
  id: string;
  origin: Origin;
  kind: MemoryKind;
  text: string;
  include: boolean;
}

export interface RitualRow {
  id: string;
  name: string;
  prompt: string;
  schedule: Schedule;
  /** Why the schedule is a guess, shown so the person checks it before turning the ritual on. */
  scheduleNote: string;
  include: boolean;
}

/** What the import sends once the person has reviewed it. */
export interface TemplateDraft {
  name: string;
  provider: Provider;
  model: string | null;
  effort: string | null;
  access: Access;
  avatar: Avatar;
  memory: { kind: MemoryKind; text: string }[];
  rituals: { name: string; prompt: string; schedule: Schedule }[];
}

/** Pieces no longer than `limit` characters, cut after a sentence or a space when one is near. */
export function splitText(text: string, limit = ENTRY_LIMIT): string[] {
  const parts: string[] = [];
  let rest = text.trim();
  while (rest.length > limit) {
    const window = rest.slice(0, limit);
    const sentence = Math.max(window.lastIndexOf(". "), window.lastIndexOf("\n"));
    const cut = sentence > limit / 2 ? sentence + 1 : window.lastIndexOf(" ") > limit / 2 ? window.lastIndexOf(" ") : limit;
    parts.push(rest.slice(0, cut).trim());
    rest = rest.slice(cut).trim();
  }
  if (rest) parts.push(rest);
  return parts;
}

/**
 * Turns a marketplace schedule written in prose into one Grimoire can run. Templates only describe
 * routines in words, so this is always a guess the person sees.
 */
export function guessSchedule(summary: string): { schedule: Schedule; note: string } {
  const text = summary.toLowerCase();
  if (/\bhourly\b|every hour/.test(text)) return { schedule: { kind: "interval", minutes: 60 }, note: "Guessed from \"hourly\"." };
  if (/\bdaily\b|every (day|morning)|each (day|morning)/.test(text)) return { schedule: { kind: "daily", time: "09:00", days: [] }, note: "Guessed from \"daily\"." };
  if (/\bweekdays?\b/.test(text)) return { schedule: { kind: "daily", time: "09:00", days: [1, 2, 3, 4, 5] }, note: "Guessed from \"weekdays\"." };
  if (/\bweekly\b|every week|each week/.test(text)) return { schedule: { kind: "daily", time: "09:00", days: [1] }, note: "Guessed from \"weekly\"." };
  if (/\bmonthly\b|every month/.test(text))
    return { schedule: { kind: "interval", minutes: 7 * 1440 }, note: "Monthly in the template. Grimoire repeats at most every 7 days, so this runs weekly." };
  return { schedule: { kind: "daily", time: "09:00", days: [1] }, note: "The template gives no schedule. Set to Mondays." };
}

const clip = (text: string, max: number) => (text.length > max ? text.slice(0, max).trimEnd() : text);

/**
 * Maps a template onto a familiar. Grimoire has no persona field: what a familiar is told at the
 * start of a chat is its memory, so the description, instructions, memories, and skills become rules,
 * and the apps it expects become one fact. Routines become rituals. Nothing becomes a tool grant.
 */
export function draftFromTemplate(t: MarketTemplate): { name: string; memory: MemoryRow[]; rituals: RitualRow[] } {
  const name = clip(t.name.trim() || t.slug, 60);
  let n = 0;
  const rows: MemoryRow[] = [];
  const add = (origin: Origin, kind: MemoryKind, text: string) => {
    for (const part of splitText(text)) rows.push({ id: `m${n++}`, origin, kind, text: part, include: true });
  };
  if (t.description.trim()) add("description", "rule", `You are ${name}. ${t.description.trim()}`);
  if (t.instructions.trim()) add("instructions", "rule", t.instructions);
  for (const m of t.memories) if (m.description.trim()) add("memory", "rule", m.description);
  for (const s of t.skills) {
    const body = s.description.trim();
    if (body) add("skill", "rule", s.name ? `When asked to ${s.name}: ${body}` : body);
  }
  if (t.integrations.length) {
    const apps = t.integrations.map((i) => i.name).filter(Boolean).join(", ");
    add("integrations", "fact", `This familiar's template expected these apps: ${apps}. Grimoire didn't connect any of them, so ask before assuming access.`);
  }
  // Rows past the memory limit start unchecked; the person picks what matters most.
  let used = 0;
  for (const row of rows) {
    const size = entriesSize([row]);
    row.include = used + size <= MEMORY_LIMIT;
    if (row.include) used += size;
  }
  const rituals = t.routines
    .filter((r) => r.name.trim() || r.description.trim())
    .map((r, i) => {
      const { schedule, note } = guessSchedule(`${r.name} ${r.description}`);
      return { id: `r${i}`, name: clip(r.name.trim() || `Ritual ${i + 1}`, 60), prompt: r.description.replace(/^\s*disabled by default\.\s*/i, "").trim() || r.name.trim(), schedule, scheduleNote: note, include: true };
    });
  return { name, memory: rows, rituals };
}

/** The included memory rows, and whether they fit the familiar's memory. */
export function memoryFit(rows: MemoryRow[]): { bytes: number; fits: boolean; tooLong: number } {
  const kept = rows.filter((r) => r.include);
  const tooLong = kept.filter((r) => r.text.trim().length > ENTRY_LIMIT || !r.text.trim()).length;
  const bytes = entriesSize(kept);
  return { bytes, fits: bytes <= MEMORY_LIMIT && tooLong === 0, tooLong };
}

/**
 * What the template asks for, in words for the preview. `connectable` names the template's apps
 * that Grimoire offers as connectors.
 */
export function permissionLines(t: MarketTemplate, access: Access, provider: Provider, connectable: string[] = []): string[] {
  const workspace =
    provider === "codex"
      ? "Full access: off. Codex runs it in a sandbox that changes files only in its workspace folder."
      : "Full access: off. Claude edits files only in its workspace, but shell commands follow your Permissions settings, which allow them unless you set Grimoire to ask.";
  const lines = [
    access === "full" ? "Full access: on. Commands can run anywhere on this Mac without a sandbox." : workspace,
    connectable.length
      ? "No tools or skills are installed. Skills come in as written rules only."
      : "No tools, skills, or connectors are installed. Skills come in as written rules only.",
  ];
  if (connectable.length) lines.push(`Connectors below can link ${connectable.join(", ")} once you sign in.`);
  const rest = t.integrations.map((i) => i.name).filter((name) => !connectable.includes(name));
  if (rest.length) lines.push(`Expects ${rest.length === 1 ? "this app" : "these apps"}, which Grimoire won't connect: ${rest.join(", ")}.`);
  if (t.routines.length) lines.push(`Rituals come in turned off. Turn each on in Rituals after you check its schedule.`);
  return lines;
}

/** Keeps search simple: every word must appear in the name, creator, description, or category. */
export function matchesSearch(t: MarketTemplate, query: string): boolean {
  const hay = `${t.name} ${t.creator} ${t.description} ${t.categories.join(" ")}`.toLowerCase();
  return query.toLowerCase().split(/\s+/).filter(Boolean).every((word) => hay.includes(word));
}
