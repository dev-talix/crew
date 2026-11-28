// Skills: SKILL.md folders in Grimoire's library, turned on per familiar. The types the backend sends,
// and the small pieces of logic the Settings page, familiar settings, and template import share.

import type { TemplateItem } from "./templates";

export type SkillSource = "you" | "folder" | "url" | "familiar" | "template";

export interface SkillFile {
  /** Relative to the skill's folder. */
  path: string;
  text: string;
}

export interface Skill {
  name: string;
  description: string;
  source: SkillSource;
  /** The folder, URL, or template page it came from. */
  origin: string | null;
  /** The familiar that made it, by id. */
  createdBy: string | null;
  /** Familiars that have it on. */
  agents: string[];
  /** File paths, SKILL.md first. */
  files: string[];
  updatedAt: number;
}

/** What adding a skill would save, with every file's full text. */
export interface SkillPreview {
  name: string;
  description: string;
  files: SkillFile[];
  notes: string[];
  source: SkillSource;
  origin: string | null;
}

export type AddFrom = { kind: "text"; text: string } | { kind: "url"; url: string } | { kind: "folder" };

/** A template skill in the import preview, before it becomes a SKILL.md. */
export interface TemplateSkillRow {
  id: string;
  name: string;
  description: string;
  body: string;
  include: boolean;
}

export const DESCRIPTION_LIMIT = 1024;
const NAME_PATTERN = /^[a-z0-9]+(-[a-z0-9]+)*$/;

/** The agentskills.io name rules: 1 to 64 lowercase letters, digits, and single hyphens. */
export function validName(name: string): boolean {
  return name.length <= 64 && NAME_PATTERN.test(name);
}

/** A skill name from free text, as the backend makes one: "Haggle Setup!" becomes haggle-setup. */
export function nameFrom(text: string): string {
  const name = text
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+/, "")
    .slice(0, 64)
    .replace(/-+$/, "");
  return name || "skill";
}

/** "Made by Scribe", "Added by you", "From github.com/…", for the library list. */
export function sourceLabel(skill: Skill, names: Record<string, string>): string {
  switch (skill.source) {
    case "familiar":
      return `Made by ${(skill.createdBy && names[skill.createdBy]) || "a deleted familiar"}`;
    case "template":
      return "From a Grok Bot template";
    case "url":
      return `From ${skill.origin?.replace(/^https:\/\//, "") ?? "a URL"}`;
    case "folder":
      return `From ${skill.origin ?? "a folder"}`;
    default:
      return "Added by you";
  }
}

/** "On for Scribe and Ada", or "Off for every familiar". */
export function enabledLabel(skill: Skill, names: Record<string, string>): string {
  const on = skill.agents.map((id) => names[id]).filter(Boolean);
  if (!on.length) return "Off for every familiar";
  return `On for ${on.length > 1 ? `${on.slice(0, -1).join(", ")} and ${on[on.length - 1]}` : on[0]}`;
}

/** The first sentence of `text`, clipped to the description limit. */
function firstSentence(text: string): string {
  const flat = text.replace(/\s+/g, " ").trim();
  const end = flat.search(/[.!?](\s|$)/);
  const sentence = end > 0 ? flat.slice(0, end + 1) : flat;
  return sentence.length > DESCRIPTION_LIMIT ? `${sentence.slice(0, DESCRIPTION_LIMIT - 1).trimEnd()}…` : sentence;
}

/**
 * Template skills as rows for the import preview. Each becomes a real skill, on for the imported
 * familiar only: the skill's title gives its name, its text the body, and the first sentence the
 * description the CLI shows when picking a skill.
 */
export function templateSkills(items: TemplateItem[]): TemplateSkillRow[] {
  return items
    .filter((s) => s.description.trim())
    .map((s, i) => {
      const body = s.description.trim();
      const title = s.name.trim();
      return {
        id: `s${i}`,
        name: nameFrom(title || `skill ${i + 1}`),
        description: title ? `Use when asked to ${title}. ${firstSentence(body)}`.slice(0, DESCRIPTION_LIMIT) : firstSentence(body),
        body,
        include: true,
      };
    });
}

/** Why a template skill row can't be imported as it stands, or null when it can. */
export function skillRowProblem(row: TemplateSkillRow): string | null {
  if (!validName(row.name)) return "Use lowercase letters, digits, and single hyphens for the name.";
  if (!row.description.trim()) return "Write what it does and when to use it.";
  if (row.description.trim().length > DESCRIPTION_LIMIT) return `Keep the description to ${DESCRIPTION_LIMIT} characters.`;
  if (!row.body.trim()) return "Write the instructions.";
  return null;
}

/** The switches flipped between `before` and `after`. */
export function skillChanges(before: string[], after: string[]): { on: string[]; off: string[] } {
  return { on: after.filter((n) => !before.includes(n)), off: before.filter((n) => !after.includes(n)) };
}
