import { expect, test } from "bun:test";
import type { Handoff } from "./api";
import { linkLabel, startedReplying, STATUS_LABELS } from "./handoffList";

const handoff = (id: string, createdAt: number, status: Handoff["status"] = "waiting", kind: Handoff["kind"] = "ask"): Handoff => ({
  id,
  fromId: "a",
  toId: "b",
  kind,
  what: id,
  createdAt,
  updatedAt: createdAt,
  status,
});

test("labels every status and the link row", () => {
  expect(Object.values(STATUS_LABELS)).toEqual(["Waiting", "Accepted", "Done", "Failed"]);
  expect(linkLabel(handoff("x", 1), "Scribe")).toBe("Asked Scribe");
  expect(linkLabel(handoff("x", 1, "done", "message"), "Scribe")).toBe("Messaged Scribe");
});

test("marks a waiting handoff accepted once its reply streams, and leaves others alone", () => {
  const list = [handoff("w", 1), handoff("d", 2, "done"), handoff("other", 3)];
  const next = startedReplying(list, "w", 50);
  expect(next.map((h) => h.status)).toEqual(["accepted", "done", "waiting"]);
  expect(next[0].updatedAt).toBe(50);
  expect(startedReplying(list, "d", 50)[1]).toBe(list[1]);
});
