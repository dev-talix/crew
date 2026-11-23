// Pure helpers for spells (workflows) and the rituals that cast them, kept apart so they can be tested.
import type { Agent, Routine, StepCheck, Workflow, WorkflowStep } from "./api";

/** Indices of the steps that start together: a step, then any after it that run alongside it. */
export function stepGroups(steps: WorkflowStep[]): number[][] {
  const groups: number[][] = [];
  steps.forEach((step, index) => {
    if (step.parallel && groups.length) groups[groups.length - 1].push(index);
    else groups.push([index]);
  });
  return groups;
}

export interface Handoff {
  from: string;
  to: string;
  /** "next" passes replies on to the next group; "check" sends the spell to the step a check names. */
  kind: "next" | "check";
}

/** Who hands work to whom: every familiar in a group to every one in the next, then each check's jump. */
export function handoffs(steps: WorkflowStep[]): Handoff[] {
  const groups = stepGroups(steps);
  const list: Handoff[] = [];
  for (let g = 1; g < groups.length; g++) {
    for (const a of groups[g - 1]) for (const b of groups[g]) list.push({ from: steps[a].agentId, to: steps[b].agentId, kind: "next" });
  }
  for (const step of steps) {
    const target = step.check ? steps[step.check.goTo] : undefined;
    if (target) list.push({ from: step.agentId, to: target.agentId, kind: "check" });
  }
  return list;
}

/** Like "If the reply lacks "APPROVED", back to step 2", for step `index`. */
export function describeCheck(check: StepCheck, index: number): string {
  const phrase = `"${check.phrase}"`;
  const condition = check.when === "contains" ? `has ${phrase}` : `lacks ${phrase}`;
  const where = check.goTo <= index ? "back to" : "on to";
  return `If the reply ${condition}, ${where} step ${check.goTo + 1}`;
}

/** "Step 2 of 4", or "Steps 2 and 3 of 4" while steps run together. */
export function progressLabel(steps: number[], total: number): string {
  if (steps.length <= 1) return `Step ${steps[0] ?? total} of ${total}`;
  return `Steps ${steps.slice(0, -1).join(", ")} and ${steps[steps.length - 1]} of ${total}`;
}

/** The familiars a ritual reaches first: its familiar, or the first steps of its spell. */
export function ritualAgents(routine: Routine, spells: Workflow[]): string[] {
  if (routine.agentId) return [routine.agentId];
  const spell = spells.find((s) => s.id === routine.workflowId);
  if (!spell) return [];
  return (stepGroups(spell.steps)[0] ?? []).map((i) => spell.steps[i].agentId);
}

const clip = (text: string, max = 60) => {
  const line = text.split("\n").find((l) => l.trim())?.trim() ?? "";
  return line.length > max ? `${line.slice(0, max - 1).trimEnd()}…` : line;
};

/** What a ritual does on each run, for the Rituals list. */
export function ritualDoes(routine: Routine, spells: Workflow[]): string {
  if (!routine.workflowId) return `Sends "${clip(routine.prompt)}"`;
  const name = spells.find((s) => s.id === routine.workflowId)?.name ?? "a deleted spell";
  return routine.prompt.trim() ? `Casts ${name}, starting with "${clip(routine.prompt, 40)}"` : `Casts ${name}`;
}

/** Who a ritual is for, for Home's "Coming up": the familiar's name, or the spell it casts. */
export function ritualWho(routine: Routine, agents: Agent[], spells: Workflow[]): string {
  if (routine.workflowId) return `Casts ${spells.find((s) => s.id === routine.workflowId)?.name ?? "a deleted spell"}`;
  return agents.find((a) => a.id === routine.agentId)?.name ?? "A deleted familiar";
}

/** Whether a reply is long enough to start collapsed. */
export function isLong(text: string): boolean {
  return text.length > 600 || text.split("\n").length > 8;
}

/** A short duration like "45s", "3m 4s", or "1h 2m". */
export function duration(ms: number): string {
  const seconds = Math.max(0, Math.round(ms / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return seconds % 60 ? `${minutes}m ${seconds % 60}s` : `${minutes}m`;
  return minutes % 60 ? `${Math.floor(minutes / 60)}h ${minutes % 60}m` : `${Math.floor(minutes / 60)}h`;
}

/** A step in the editor. Checks point at another step's key, so they follow it when steps move. */
export interface DraftStep {
  key: number;
  agentId: string;
  prompt: string;
  parallel: boolean;
  check: { when: StepCheck["when"]; phrase: string; target: number | null } | null;
}

export function toDrafts(steps: WorkflowStep[], key: () => number): DraftStep[] {
  const keys = steps.map(() => key());
  return steps.map((step, i) => ({
    key: keys[i],
    agentId: step.agentId,
    prompt: step.prompt,
    parallel: !!step.parallel && i > 0,
    check: step.check ? { when: step.check.when, phrase: step.check.phrase, target: keys[step.check.goTo] ?? null } : null,
  }));
}

/** The steps to save, or a message when a check has lost the step it went to. */
export function fromDrafts(drafts: DraftStep[]): WorkflowStep[] | string {
  const steps: WorkflowStep[] = [];
  for (const [i, draft] of drafts.entries()) {
    let check: StepCheck | null = null;
    if (draft.check) {
      const goTo = drafts.findIndex((d) => d.key === draft.check!.target);
      if (goTo < 0) return `Step ${i + 1}'s check needs a step to go to.`;
      check = { when: draft.check.when, phrase: draft.check.phrase, goTo };
    }
    // The first step has no step to run alongside, and the editor hides the choice there.
    steps.push({ agentId: draft.agentId, prompt: draft.prompt, parallel: draft.parallel && i > 0, check });
  }
  return steps;
}
