import { describe, expect, test } from "bun:test";
import type { RoomMessage } from "./api";
import {
  appendRoomChunk,
  completeMention,
  floorLine,
  mentionMatches,
  mentionQuery,
  mentionSegments,
  mergeRoomSnapshot,
  upsertRoomMessage,
} from "./roomState";

const members = [
  { agentId: "a", name: "Ink" },
  { agentId: "b", name: "Ink Well" },
  { agentId: "c", name: "Scribe" },
];

const row = (id: number, text: string, status: RoomMessage["status"] = "done"): RoomMessage => ({
  id,
  roomId: "r",
  agentId: "c",
  name: "Scribe",
  kind: "reply",
  text,
  status,
  targetId: null,
  targetName: null,
  hasPrompt: false,
  createdAt: 0,
});

describe("mentions", () => {
  test("match whole names in any case, the longest first, never inside a word", () => {
    expect(mentionSegments("hi @scribe and @Ink Well, me@Scribe.com @Inkling", members)).toEqual([
      { text: "hi " },
      { text: "@scribe", memberId: "c" },
      { text: " and " },
      { text: "@Ink Well", memberId: "b" },
      { text: ", me@Scribe.com @Inkling" },
    ]);
  });

  test("finds the @ word being typed and completes it", () => {
    expect(mentionQuery("hey @Sc")).toEqual({ start: 4, query: "Sc" });
    expect(mentionQuery("@")).toEqual({ start: 0, query: "" });
    expect(mentionQuery("mail me@sc")).toBeNull();
    expect(mentionQuery("@Scribe done.")).toBeNull();
    expect(mentionMatches("ink", members).map((m) => m.agentId)).toEqual(["a", "b"]);
    expect(completeMention("hey @Sc please", 7, "Scribe")).toEqual({ text: "hey @Scribe please", caret: 12 });
    expect(completeMention("", 0, "Ink Well")).toEqual({ text: "@Ink Well ", caret: 10 });
    expect(completeMention("Please look", 11, "Scribe")).toEqual({ text: "Please look @Scribe ", caret: 20 });
    expect(completeMention("請確認𠮷", 5, "Scribe").text).toBe("請確認𠮷 @Scribe ");
    expect(mentionSegments("𠮷@Scribe", members)).toEqual([{ text: "𠮷@Scribe" }]);
  });
});

describe("room messages", () => {
  test("stream into the reply and keep the newer copy when history loads late", () => {
    let list = upsertRoomMessage([], row(2, "Hel", "streaming"));
    list = upsertRoomMessage(list, row(1, "first"));
    expect(list.map((m) => m.id)).toEqual([1, 2]);
    list = appendRoomChunk(list, 2, "lo");
    expect(list[1].text).toBe("Hello");
    const done = upsertRoomMessage(list, row(2, "Hello there"));
    expect(appendRoomChunk(done, 2, "!")[1].text).toBe("Hello there");
    expect(mergeRoomSnapshot([row(1, "first"), row(2, "Hel", "streaming")], [row(2, "Hello there")])[1].text).toBe("Hello there");
    expect(mergeRoomSnapshot([row(2, "Hello there")], [row(2, "Hel", "streaming")])[0].text).toBe("Hello there");
  });

  test("say who has the floor and who waits", () => {
    const names: Record<string, string> = { a: "Researcher", b: "Scribe", c: "Critic" };
    expect(floorLine(null, ["a"], (id) => names[id])).toBe("");
    expect(floorLine("a", [], (id) => names[id])).toBe("Researcher is replying");
    expect(floorLine("a", ["b", "c"], (id) => names[id])).toBe("Researcher is replying, then Scribe, Critic");
  });
});
