import { For } from "solid-js";
import BotAvatar from "./BotAvatar";
import type { Agent, Schedule } from "./api";
import type { Avatar } from "./avatar";

/** Starting points offered when a page is empty. */
export interface RitualTemplate {
  name: string;
  prompt: string;
  schedule: Schedule;
  when: string;
}

export interface SpellTemplate {
  name: string;
  steps: string[];
  about: string;
}

export const RITUAL_TEMPLATES: RitualTemplate[] = [
  {
    name: "Morning briefing",
    prompt: "Summarize what changed in my workspace since yesterday and list anything that needs my attention.",
    schedule: { kind: "daily", time: "09:00", days: [1, 2, 3, 4, 5] },
    when: "Weekdays at 9 AM",
  },
  {
    name: "Hourly check",
    prompt: "Check whether the tests still pass. Only report if something broke, and say what.",
    schedule: { kind: "interval", minutes: 60 },
    when: "Every hour",
  },
  {
    name: "Friday wrap-up",
    prompt: "Write a short review of this week's work in the workspace: what got done, what's stuck, and what's next.",
    schedule: { kind: "daily", time: "16:00", days: [5] },
    when: "Fridays at 4 PM",
  },
];

export const SPELL_TEMPLATES: SpellTemplate[] = [
  {
    name: "Research, then write",
    steps: ["Research {{input}} and list the key facts, with sources.", "Write a short, clear brief from this research."],
    about: "Two steps",
  },
  {
    name: "Draft and review",
    steps: ["Draft {{input}}.", "Review this draft for mistakes and unclear parts, then return a corrected version."],
    about: "Two steps",
  },
  {
    name: "Plan, build, check",
    steps: [
      "Plan how to do {{input}} in small steps.",
      "Carry out the plan, one step at a time.",
      "Check the work: run the tests and list anything that still needs fixing.",
    ],
    about: "Three steps",
  },
];

const FALLBACK: Avatar[] = [
  { shape: "orbit", eyes: "pill", color: "#8e5bd6" },
  { shape: "spark", eyes: "pill", color: "#dc9a32" },
  { shape: "ears", eyes: "dot", color: "#2896a6" },
];

const avatars = (agents: Agent[], count: number) =>
  Array.from({ length: count }, (_, i) => agents[i % Math.max(agents.length, 1)]?.avatar ?? FALLBACK[i % FALLBACK.length]);

/** A familiar inside a ritual dial. */
export function RitualArt(props: { agents: Agent[] }) {
  return (
    <div class="empty-art ritual-art" aria-hidden="true">
      <svg viewBox="0 0 120 120" class="empty-art-lines">
        <circle cx="60" cy="60" r="50" />
        <circle cx="60" cy="60" r="42" stroke-dasharray="1 5" />
        <For each={Array.from({ length: 12 }, (_, i) => i)}>
          {(i) => {
            const a = (i * 30 * Math.PI) / 180;
            const [c, s] = [Math.cos(a), Math.sin(a)];
            return <line x1={60 + 50 * c} y1={60 + 50 * s} x2={60 + (i % 3 ? 46 : 43) * c} y2={60 + (i % 3 ? 46 : 43) * s} />;
          }}
        </For>
      </svg>
      <BotAvatar avatar={avatars(props.agents, 1)[0]} size={54} />
    </div>
  );
}

/** Three familiars passing a spell along a line. */
export function SpellArt(props: { agents: Agent[] }) {
  return (
    <div class="empty-art spell-art" aria-hidden="true">
      <svg viewBox="0 0 200 60" class="empty-art-lines">
        <path d="M30 30H170" stroke-dasharray="3 5" />
        <path class="empty-art-spark" d="M65 18l2 5 5 2-5 2-2 5-2-5-5-2 5-2zM135 36l1.6 4 4 1.6-4 1.6-1.6 4-1.6-4-4-1.6 4-1.6z" />
      </svg>
      <For each={avatars(props.agents, 3)}>
        {(avatar) => (
          <span class="empty-art-badge">
            <BotAvatar avatar={avatar} size={36} />
          </span>
        )}
      </For>
    </div>
  );
}
