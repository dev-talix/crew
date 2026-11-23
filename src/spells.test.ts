import { expect, test } from "bun:test";
import type { Routine, Workflow, WorkflowStep } from "./api";
import {
  describeCheck,
  duration,
  fromDrafts,
  handoffs,
  isLong,
  progressLabel,
  ritualAgents,
  ritualDoes,
  ritualWho,
  stepGroups,
  toDrafts,
} from "./spells";

const step = (agentId: string, extra: Partial<WorkflowStep> = {}): WorkflowStep => ({ agentId, prompt: agentId, ...extra });

test("steps that run alongside join the group before them", () => {
  expect(stepGroups([step("a"), step("b"), step("c", { parallel: true }), step("d")])).toEqual([[0], [1, 2], [3]]);
  // Older spells have no parallel flag at all.
  expect(stepGroups([{ agentId: "a", prompt: "" }, { agentId: "b", prompt: "" }])).toEqual([[0], [1]]);
});

test("handoffs fan out to a parallel group, join after it, and follow checks", () => {
  const steps = [
    step("w"),
    step("e", { check: { when: "lacks", phrase: "ok", goTo: 0 } }),
    step("a", { parallel: true }),
    step("p"),
  ];
  expect(handoffs(steps).map((h) => `${h.from}>${h.to}:${h.kind}`)).toEqual(["w>e:next", "w>a:next", "e>p:next", "a>p:next", "e>w:check"]);
  // A check pointing past the end draws nothing.
  expect(handoffs([step("a", { check: { when: "contains", phrase: "x", goTo: 5 } })])).toEqual([]);
});

test("labels checks and progress", () => {
  expect(describeCheck({ when: "lacks", phrase: "APPROVED", goTo: 0 }, 2)).toBe('If the reply lacks "APPROVED", back to step 1');
  expect(describeCheck({ when: "contains", phrase: "skip", goTo: 3 }, 0)).toBe('If the reply has "skip", on to step 4');
  expect(progressLabel([2], 4)).toBe("Step 2 of 4");
  expect(progressLabel([2, 3], 4)).toBe("Steps 2 and 3 of 4");
  expect(progressLabel([1, 2, 3], 3)).toBe("Steps 1, 2 and 3 of 3");
});

const spell: Workflow = { id: "s", name: "Topic brief", steps: [step("a"), step("b", { parallel: true }), step("c")], createdAt: 0, createdBy: null };
const ritual = (extra: Partial<Routine>): Routine => ({
  id: "r",
  agentId: null,
  workflowId: null,
  name: "Morning",
  prompt: "",
  schedule: { kind: "interval", minutes: 60 },
  enabled: true,
  lastRunAt: null,
  nextRunAt: 1,
  createdAt: 0,
  ...extra,
});

test("describes what a ritual does", () => {
  const agents = [{ id: "a", name: "Ada" }] as never;
  const message = ritual({ agentId: "a", prompt: "Summarize the repo\nand more" });
  expect(ritualDoes(message, [spell])).toBe('Sends "Summarize the repo"');
  expect(ritualWho(message, agents, [spell])).toBe("Ada");
  expect(ritualAgents(message, [spell])).toEqual(["a"]);

  const cast = ritual({ workflowId: "s" });
  expect(ritualDoes(cast, [spell])).toBe("Casts Topic brief");
  expect(ritualDoes({ ...cast, prompt: "AI news" }, [spell])).toBe('Casts Topic brief, starting with "AI news"');
  expect(ritualWho(cast, agents, [spell])).toBe("Casts Topic brief");
  expect(ritualAgents(cast, [spell])).toEqual(["a", "b"]);
  expect(ritualDoes(cast, [])).toBe("Casts a deleted spell");
  expect(ritualAgents(cast, [])).toEqual([]);
});

test("drafts keep check targets when steps move, and catch lost ones", () => {
  let key = 10;
  const drafts = toDrafts([step("w"), step("e", { check: { when: "lacks", phrase: "ok", goTo: 0 } })], () => key++);
  expect(drafts[1].check?.target).toBe(10);
  const moved = [drafts[1], drafts[0]];
  expect(fromDrafts(moved)).toEqual([
    { agentId: "e", prompt: "e", parallel: false, check: { when: "lacks", phrase: "ok", goTo: 1 } },
    { agentId: "w", prompt: "w", parallel: false, check: null },
  ]);
  expect(fromDrafts([drafts[1]])).toBe("Step 1's check needs a step to go to.");
  // A parallel step moved to the top runs first on its own.
  expect((fromDrafts([{ ...drafts[0], parallel: true }]) as WorkflowStep[])[0].parallel).toBe(false);
});

test("formats durations and spots long text", () => {
  expect(duration(4_400)).toBe("4s");
  expect(duration(184_000)).toBe("3m 4s");
  expect(duration(120_000)).toBe("2m");
  expect(duration(3_720_000)).toBe("1h 2m");
  expect(isLong("short")).toBe(false);
  expect(isLong("x".repeat(700))).toBe(true);
  expect(isLong("a\n".repeat(10))).toBe(true);
});
