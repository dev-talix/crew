import { expect, test } from "bun:test";
import type { Workflow } from "./api";
import { seat, spellLinks } from "./circle";

const spell = (id: string, agents: string[]): Workflow => ({
  id,
  name: id,
  steps: agents.map((agentId) => ({ agentId, prompt: "" })),
  createdAt: 0,
  createdBy: null,
});

test("seats start at the top and go around evenly", () => {
  const top = seat(0, 4);
  expect(top.x).toBeCloseTo(50);
  expect(top.y).toBeLessThan(50);
  const right = seat(1, 4);
  expect(right.x).toBeGreaterThan(50);
  expect(right.y).toBeCloseTo(50);
});

test("spells link the familiars they hand work between", () => {
  const links = spellLinks(
    [spell("brief", ["a", "b", "b", "c"]), spell("check", ["b", "a", "b"]), spell("gone", ["a", "zombie"])],
    ["a", "b", "c"],
  );
  expect(links.map((l) => `${l.spell.id}:${l.from}>${l.to}:${l.lane}`)).toEqual(["brief:a>b:0", "brief:b>c:0", "check:b>a:1"]);
});

test("coven sectors cover each run of neighbors in the same coven", async () => {
  const { covenSectors } = await import("./circle");
  const sectors = covenSectors(["r", "r", null, "o"]);
  expect(sectors.map((s) => s.covenId)).toEqual(["r", "o"]);
  // Four seats are 90 degrees apart, starting at the top; the first sector spans seats 0 and 1.
  expect(sectors[0].start).toBeCloseTo(-90 - 45 + 4);
  expect(sectors[0].end).toBeCloseTo(0 + 45 - 4);
  expect(covenSectors([null, null])).toEqual([]);
});
