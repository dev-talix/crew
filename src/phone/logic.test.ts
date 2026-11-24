import { describe, expect, test } from "bun:test";
import type { Draft, InboxItem, Message } from "../api";
import { applyChatEvent, backoff, draftAlert, inboxAlert, mergeHistory, pendingDrafts, phoneOptions, replyAlert, sortInbox, titleText } from "./logic";

const msg = (id: number, text: string, extra: Partial<Message> = {}): Message => ({
  id,
  agentId: "a1",
  role: "assistant",
  text,
  status: "done",
  source: null,
  createdAt: id,
  updatedAt: null,
  ...extra,
});

const item = (id: string, kind: InboxItem["kind"], createdAt: number | null = null): InboxItem => ({
  id,
  kind,
  agentId: "a1",
  title: `Title ${id}`,
  detail: "",
  options:
    kind === "question"
      ? [
          { value: "x", label: "X" },
          { value: "y", label: "Y" },
        ]
      : [
          { value: "once", label: "Allow once" },
          { value: "always", label: "Always allow" },
          { value: "deny", label: "Deny" },
        ],
  recommended: kind === "question" ? "y" : "once",
  createdAt,
});

const draft = (id: string, state: Draft["state"]): Draft => ({
  id,
  agentId: "a1",
  channel: "email",
  to: ["a@example.com"],
  subject: "Hi",
  body: "Hello",
  note: null,
  state,
  createdAt: 1,
  closedAt: null,
});

const nameOf = (id: string) => (id === "a1" ? "Wren" : "Moss");

describe("chat events", () => {
  test("appends streamed chunks to the message they name", () => {
    const start = [msg(1, "hi", { role: "user" }), msg(2, "", { status: "streaming" })];
    const after = applyChatEvent(applyChatEvent(start, "a1", { kind: "delta", payload: { agentId: "a1", messageId: 2, chunk: "Hel" } }), "a1", {
      kind: "delta",
      payload: { agentId: "a1", messageId: 2, chunk: "lo" },
    });
    expect(after.map((m) => m.text)).toEqual(["hi", "Hello"]);
  });

  test("a message event replaces the streamed copy, or adds a new one in id order", () => {
    const start = [msg(1, "a"), msg(3, "partial", { status: "streaming" })];
    const replaced = applyChatEvent(start, "a1", { kind: "message", payload: msg(3, "final") });
    expect(replaced.map((m) => [m.id, m.text, m.status])).toEqual([
      [1, "a", "done"],
      [3, "final", "done"],
    ]);
    const added = applyChatEvent(replaced, "a1", { kind: "message", payload: msg(2, "b") });
    expect(added.map((m) => m.id)).toEqual([1, 2, 3]);
  });

  test("ignores events for another familiar", () => {
    const start = [msg(1, "a")];
    expect(applyChatEvent(start, "a1", { kind: "message", payload: msg(2, "b", { agentId: "a2" }) })).toEqual(start);
    expect(applyChatEvent(start, "a1", { kind: "delta", payload: { agentId: "a2", messageId: 1, chunk: "!" } })).toEqual(start);
  });
});

describe("inbox", () => {
  test("approvals offer only Allow once and Deny", () => {
    expect(phoneOptions(item("p", "command")).map((o) => o.value)).toEqual(["once", "deny"]);
  });

  test("questions keep every option, recommended first", () => {
    expect(phoneOptions(item("q", "question")).map((o) => o.value)).toEqual(["y", "x"]);
  });

  test("approvals come before questions, questions oldest first", () => {
    const sorted = sortInbox([item("q2", "question", 20), item("q1", "question", 10), item("p", "command")]);
    expect(sorted.map((i) => i.id)).toEqual(["p", "q1", "q2"]);
  });

  test("the tab title counts what waits", () => {
    expect(titleText(0)).toBe("Grimoire");
    expect(titleText(3)).toBe("(3) Grimoire");
  });

  test("pending drafts leave out sent and discarded ones", () => {
    expect(pendingDrafts([draft("1", "draft"), draft("2", "sent"), draft("3", "discarded")]).map((d) => d.id)).toEqual(["1"]);
  });
});

describe("alerts", () => {
  test("no banner on the first load", () => {
    expect(inboxAlert(null, [item("q", "question")], nameOf)).toBeNull();
    expect(draftAlert(null, [draft("1", "draft")], nameOf)).toBeNull();
  });

  test("a banner for a new inbox item, approvals named first", () => {
    expect(inboxAlert(new Set(["old"]), [item("old", "question"), item("q", "question")], nameOf)).toBe("Wren asks: Title q");
    expect(inboxAlert(new Set(), [item("q", "question"), item("p", "command")], nameOf)).toBe("Wren needs your OK (and 1 more)");
    expect(inboxAlert(new Set(["q"]), [item("q", "question")], nameOf)).toBeNull();
  });

  test("a banner for a new pending draft only", () => {
    expect(draftAlert(new Set(), [draft("1", "draft")], nameOf)).toBe("Wren wrote a draft");
    expect(draftAlert(new Set(), [draft("1", "sent")], nameOf)).toBeNull();
    expect(draftAlert(new Set(), [draft("1", "draft"), draft("2", "draft")], nameOf)).toBe("2 new drafts to send");
  });

  test("a finished reply alerts unless that chat is open", () => {
    const done = { kind: "status", payload: { agentId: "a2", busy: false } } as const;
    expect(replyAlert(done, null, nameOf)).toBe("Moss replied");
    expect(replyAlert(done, "a2", nameOf)).toBeNull();
    expect(replyAlert({ kind: "status", payload: { agentId: "a2", busy: true } }, null, nameOf)).toBeNull();
  });
});

test("backoff doubles from 2 seconds and stops at 30", () => {
  expect([0, 1, 2, 3, 4, 10].map(backoff)).toEqual([2000, 4000, 8000, 16000, 30000, 30000]);
});

describe("mergeHistory", () => {
  test("keeps a reply that arrived while the history loaded", () => {
    const history = [msg(1, "hi"), msg(2, "old")];
    const live = [msg(3, "live reply")];
    expect(mergeHistory(history, live).map((m) => m.text)).toEqual(["hi", "old", "live reply"]);
  });

  test("a finished reply beats an older streaming copy", () => {
    const history = [msg(1, "Final reply", { status: "done" })];
    const live = [msg(1, "Fin", { status: "streaming" })];
    expect(mergeHistory(history, live)[0].text).toBe("Final reply");
  });

  test("a longer streaming or newer live copy wins over the history's", () => {
    const history = [msg(1, "par", { status: "streaming" }), msg(2, "draft", { updatedAt: 5 })];
    const live = [msg(1, "partial", { status: "streaming" }), msg(2, "stale", { updatedAt: 2 })];
    expect(mergeHistory(history, live).map((m) => m.text)).toEqual(["partial", "draft"]);
  });
});
