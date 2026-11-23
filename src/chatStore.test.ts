import { beforeEach, expect, mock, test } from "bun:test";
import { createRoot, createSignal } from "solid-js";
import type { Message } from "./api";

// Pages served by the fake backend, keyed by agent id. `gate` holds a load open until released.
let pages: Record<string, Message[]> = {};
let gate: Promise<void> | null = null;

mock.module("./api", () => ({
  PAGE_SIZE: 100,
  api: {
    listMessages: async (agentId: string, beforeId?: number) => {
      await gate;
      const all = pages[agentId] ?? [];
      return beforeId === undefined ? all.filter((m) => m.id >= 10) : all.filter((m) => m.id < beforeId);
    },
  },
}));

const { createChatStore } = await import("./chatStore");

const msg = (id: number, text: string, agentId = "a"): Message => ({
  id,
  agentId,
  role: "assistant",
  text,
  status: "done",
  source: null,
  createdAt: id,
  updatedAt: id,
});

// Lets pending loads resolve and any batched writes apply.
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

function setup(initial: string | null = "a") {
  return createRoot((dispose) => {
    const [selected, setSelected] = createSignal<string | null>(initial);
    return { store: createChatStore(selected), setSelected, dispose };
  });
}

const texts = (store: ReturnType<typeof createChatStore>) => store.messages.map((m) => `${m.id}:${m.text}`);

beforeEach(() => {
  pages = { a: [msg(10, "hi"), msg(11, "there")], b: [msg(20, "other", "b")] };
  gate = null;
});

test("loads the selected agent's messages", async () => {
  const { store, dispose } = setup();
  await settle();
  expect(texts(store)).toEqual(["10:hi", "11:there"]);
  dispose();
});

test("a delta right after a new message lands on that message", async () => {
  const { store, dispose } = setup();
  await settle();
  store.onMessage(msg(12, ""));
  store.onDelta({ agentId: "a", messageId: 12, chunk: "Hel" });
  store.onDelta({ agentId: "a", messageId: 12, chunk: "lo" });
  await settle();
  expect(texts(store)).toEqual(["10:hi", "11:there", "12:Hello"]);
  dispose();
});

test("a finished message replaces its streamed copy", async () => {
  const { store, dispose } = setup();
  await settle();
  store.onMessage(msg(12, ""));
  store.onDelta({ agentId: "a", messageId: 12, chunk: "draft" });
  store.onMessage(msg(12, "final"));
  await settle();
  expect(texts(store)).toEqual(["10:hi", "11:there", "12:final"]);
  dispose();
});

test("an out-of-order message is sorted into place", async () => {
  const { store, dispose } = setup();
  await settle();
  store.onMessage(msg(14, "late"));
  store.onMessage(msg(13, "early"));
  await settle();
  expect(texts(store)).toEqual(["10:hi", "11:there", "13:early", "14:late"]);
  dispose();
});

test("events during a load apply after the page arrives", async () => {
  let release!: () => void;
  gate = new Promise((resolve) => (release = resolve));
  const { store, dispose } = setup();
  store.onMessage(msg(12, ""));
  store.onDelta({ agentId: "a", messageId: 12, chunk: "queued" });
  release();
  await settle();
  expect(texts(store)).toEqual(["10:hi", "11:there", "12:queued"]);
  dispose();
});

test("switching agents drops the old agent's events and messages", async () => {
  const { store, setSelected, dispose } = setup();
  await settle();
  setSelected("b");
  store.onMessage(msg(12, "for a"));
  await settle();
  expect(texts(store)).toEqual(["20:other"]);
  dispose();
});

test("loadOlder prepends the previous page", async () => {
  pages.a = [msg(8, "old"), msg(9, "older"), ...pages.a];
  const { store, dispose } = setup();
  await settle();
  await store.loadOlder();
  await settle();
  expect(texts(store)).toEqual(["8:old", "9:older", "10:hi", "11:there"]);
  dispose();
});
