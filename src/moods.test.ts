import { expect, test } from "bun:test";
import { createRoot } from "solid-js";
import type { Message } from "./api";
import { createMoods } from "./moods";

(globalThis as { window?: unknown }).window ??= globalThis;

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

const error: Message = {
  id: 1,
  agentId: "a",
  role: "error",
  text: "boom",
  status: "done",
  source: null,
  createdAt: 1,
  updatedAt: 1,
};

function setup() {
  return createRoot((dispose) => ({ moods: createMoods(), dispose }));
}

test("a busy agent is working", () => {
  const { moods, dispose } = setup();
  expect(moods.mood("a", true)).toBe("working");
  dispose();
});

test("finishing a turn makes the agent happy", async () => {
  const { moods, dispose } = setup();
  moods.onStatus("a", true);
  moods.onStatus("a", false);
  await settle();
  expect(moods.mood("a", false)).toBe("happy");
  expect(moods.mood("b", false)).toBe("idle");
  dispose();
});

test("an error just before the turn ends stays concerned", async () => {
  const { moods, dispose } = setup();
  moods.onStatus("a", true);
  moods.onMessage(error);
  moods.onStatus("a", false);
  await settle();
  expect(moods.mood("a", false)).toBe("concerned");
  dispose();
});

test("starting a new turn clears the last mood", async () => {
  const { moods, dispose } = setup();
  moods.onMessage(error);
  moods.onStatus("a", true);
  moods.onStatus("a", false);
  await settle();
  expect(moods.mood("a", false)).toBe("happy");
  dispose();
});
