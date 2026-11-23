import { expect, test } from "bun:test";
import type { Agent, ModelOption } from "./api";
import { modelName, runningModelName } from "./models";

const codex: ModelOption[] = [
  { id: "gpt-6.2-sol", name: "GPT-6.2-Sol", description: "", efforts: ["low"], defaultEffort: "low", isDefault: true },
];

test("model ids read as product names", () => {
  expect(modelName("claude-opus-5-5")).toBe("Opus 5.5");
  expect(modelName("sonnet")).toBe("Sonnet");
  expect(modelName("gpt-6.2-sol", codex)).toBe("GPT-6.2-Sol");
  expect(modelName("gpt-6.1-terra")).toBe("GPT-6.1-Terra");
  expect(modelName("custom-model-x")).toBe("custom-model-x");
});

test("the running model prefers the choice, then what the agent reported, then the default", () => {
  const agent = { model: null, activeModel: null } as unknown as Agent;
  expect(runningModelName(agent, codex)).toBe("GPT-6.2-Sol");
  expect(runningModelName({ ...agent, activeModel: "claude-haiku-5-5" }, codex)).toBe("Haiku 5.5");
  expect(runningModelName({ ...agent, model: "opus", activeModel: "claude-haiku-5-5" })).toBe("Opus");
  expect(runningModelName({ ...agent, model: "opus", activeModel: "claude-opus-5-5" })).toBe("Opus 5.5");
});
