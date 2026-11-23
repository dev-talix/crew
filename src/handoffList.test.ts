import { expect, test } from "bun:test";
import type { Handoff } from "./api";
import { linkLabel, STATUS_LABELS } from "./handoffList";

const handoff = (status: Handoff["status"], kind: Handoff["kind"]): Handoff => ({
  id: 1,
  chatId: "c",
  fromId: "a",
  toId: "b",
  kind,
  what: "x",
  createdAt: 1,
  status,
});

test("labels every status and the link row", () => {
  expect(Object.values(STATUS_LABELS)).toEqual(["Waiting", "Accepted", "Done", "Failed"]);
  expect(linkLabel(handoff("waiting", "ask"), "Scribe")).toBe("Asked Scribe");
  expect(linkLabel(handoff("done", "message"), "Scribe")).toBe("Messaged Scribe");
});
