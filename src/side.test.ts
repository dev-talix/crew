import { describe, expect, test } from "bun:test";
import type { Message } from "./api";
import { appendChunk, mergeSnapshot, partnerName, sideState, upsert } from "./side";

const message = (id: number, role: Message["role"], text: string, status: Message["status"] = "done"): Message => ({
  id,
  agentId: "b",
  role,
  text,
  status,
  source: null,
  createdAt: id,
  updatedAt: null,
});

describe("side conversations", () => {
  test("upserts messages and streams text into a reply", () => {
    let list = upsert([], message(1, "user", "What do you know?"));
    list = upsert(list, message(2, "assistant", "Ot", "streaming"));
    list = appendChunk(list, 2, "ters");
    expect(list.map((m) => m.text)).toEqual(["What do you know?", "Otters"]);
    list = upsert(list, message(2, "assistant", "Otters hold hands."));
    expect(list).toHaveLength(2);
    expect(list[1].text).toBe("Otters hold hands.");
  });

  test("keeps live messages that beat the loaded history", () => {
    const snapshot = [message(1, "user", "hi"), message(2, "assistant", "Hel", "streaming")];
    const live = [message(2, "assistant", "Hello there."), message(3, "tool", "$ ls")];
    expect(mergeSnapshot(snapshot, live).map((m) => [m.id, m.text, m.status])).toEqual([
      [1, "hi", "done"],
      [2, "Hello there.", "done"],
      [3, "$ ls", "done"],
    ]);
    // A stale streaming copy that arrived first doesn't undo the finished history.
    const stale = [message(2, "assistant", "He", "streaming")];
    expect(mergeSnapshot([message(2, "assistant", "Hello.")], stale)[0].text).toBe("Hello.");
  });

  test("tells where a conversation stands", () => {
    expect(sideState([])).toBe("empty");
    expect(sideState([message(1, "user", "hi")])).toBe("working");
    expect(sideState([message(1, "user", "hi"), message(2, "tool", "$ ls")])).toBe("working");
    expect(sideState([message(1, "user", "hi"), message(2, "assistant", "he", "streaming")])).toBe("working");
    expect(sideState([message(1, "user", "hi"), message(2, "assistant", "hello")])).toBe("replied");
    expect(sideState([message(1, "user", "hi"), message(2, "error", "offline")])).toBe("failed");
  });

  test("reads the other familiar's name from the link row", () => {
    expect(partnerName("Asked Scribe")).toBe("Scribe");
    expect(partnerName("Messaged Warden Two")).toBe("Warden Two");
  });
});
