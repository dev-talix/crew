import { expect, test } from "bun:test";
import type { InboxItem } from "./api";
import { groupByFamiliar, optionsInOrder } from "./inboxList";

const item = (id: string, agentId: string, kind: InboxItem["kind"], createdAt: number | null): InboxItem => ({
  id,
  kind,
  agentId,
  title: id,
  detail: "",
  options: [
    { value: "a", label: "A" },
    { value: "b", label: "B" },
    { value: "c", label: "C" },
  ],
  recommended: "b",
  createdAt,
});

test("puts the recommended option first and keeps the rest in order", () => {
  expect(optionsInOrder(item("q", "x", "question", 1)).map((o) => o.value)).toEqual(["b", "a", "c"]);
  const missing = { ...item("q", "x", "question", 1), recommended: "zzz" };
  expect(optionsInOrder(missing).map((o) => o.value)).toEqual(["a", "b", "c"]);
});

test("groups by familiar with approvals first, then questions oldest first", () => {
  const groups = groupByFamiliar([
    item("q-new", "scribe", "question", 30),
    item("q-old", "scribe", "question", 10),
    item("q-mid", "seer", "question", 20),
    item("cmd", "seer", "command", null),
  ]);
  expect(groups.map((g) => g.agentId)).toEqual(["seer", "scribe"]);
  expect(groups[0].items.map((i) => i.id)).toEqual(["cmd", "q-mid"]);
  expect(groups[1].items.map((i) => i.id)).toEqual(["q-old", "q-new"]);
  expect(groupByFamiliar([])).toEqual([]);
});
