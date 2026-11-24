/**
 * DEV ONLY. A fake companion server for `bun run dev`, so /phone.html runs without the Mac.
 * vite.config.ts loads it in the dev server only. Nothing in the app imports it, so it never ships.
 * Pair with the code ABCD2345. State lives in memory and resets when the dev server restarts.
 */
import type { Draft, InboxItem, Message } from "../api";
import type { Familiar } from "./api";
import type { PhoneEvent } from "./logic";

export interface MockResponse {
  status: number;
  body: unknown;
}

const now = Date.now();
const tokens = new Set<string>();
const epoch = `mock-${now}`;

const familiars: Familiar[] = [
  { id: "wren", name: "Wren", provider: "claude", model: "claude-sonnet-5-5", avatar: { shape: "owl", eyes: "pill", color: "#8e5bd6" } },
  { id: "moss", name: "Moss", provider: "codex", model: "gpt-6.2-sol", avatar: { shape: "cauldron", eyes: "dot", color: "#2f9e76" } },
  { id: "ember", name: "Ember", provider: "claude", model: "claude-haiku-5-5", avatar: { shape: "candle", eyes: "slit", color: "#e46f4c" } },
];
const busy = new Set<string>();

let inbox: InboxItem[] = [
  {
    id: "ap-1",
    kind: "command",
    agentId: "moss",
    title: "Run a command in grimoire",
    detail: "bun run build && git status --short",
    options: [
      { value: "once", label: "Allow once" },
      { value: "always", label: "Always allow" },
      { value: "deny", label: "Deny" },
    ],
    recommended: "once",
    createdAt: null,
  },
  {
    id: "q-1",
    kind: "question",
    agentId: "wren",
    title: "Which day works for the dentist?",
    detail: "They have openings Tuesday at 9:30 and Thursday at 2:00.",
    options: [
      { value: "Tuesday 9:30", label: "Tuesday 9:30" },
      { value: "Thursday 2:00", label: "Thursday 2:00" },
    ],
    recommended: "Thursday 2:00",
    createdAt: now - 40 * 60_000,
  },
  {
    id: "q-2",
    kind: "question",
    agentId: "ember",
    title: "Should the weekly digest include GitHub activity?",
    detail: "",
    options: [
      { value: "yes", label: "Yes" },
      { value: "no", label: "No" },
    ],
    recommended: "yes",
    createdAt: now - 5 * 60_000,
  },
];

let drafts: Draft[] = [
  {
    id: "d-1",
    agentId: "wren",
    channel: "email",
    to: ["sam@example.com"],
    subject: "Dinner on Friday",
    body: "Hi Sam,\n\nAre we still on for Friday at 7? I booked the table by the window.\n\nSee you then.",
    note: "You asked me to confirm dinner plans.",
    state: "draft",
    createdAt: now - 20 * 60_000,
    closedAt: null,
  },
  {
    id: "d-2",
    agentId: "moss",
    channel: "slack",
    to: ["#eng"],
    subject: null,
    body: "Build is green again. The flaky test was a timezone issue, fixed in #42.",
    note: null,
    state: "draft",
    createdAt: now - 8 * 60_000,
    closedAt: null,
  },
  {
    id: "d-3",
    agentId: "wren",
    channel: "email",
    to: ["landlord@example.com"],
    subject: "Boiler service",
    body: "Hello,\n\nThe boiler service on Monday works for me.\n\nThanks.",
    note: null,
    state: "sent",
    createdAt: now - 26 * 3_600_000,
    closedAt: now - 25 * 3_600_000,
  },
];

let nextId = 100;
const message = (agentId: string, role: Message["role"], text: string, minutesAgo = 0, status: Message["status"] = "done"): Message => ({
  id: nextId++,
  agentId,
  role,
  text,
  status,
  source: null,
  createdAt: Date.now() - minutesAgo * 60_000,
  updatedAt: null,
});

const messages: Message[] = [
  message("wren", "user", "Can you find a time for the dentist next week?", 45),
  message("wren", "assistant", "I found two openings: Tuesday at 9:30 and Thursday at 2:00.\nI put the question in your inbox.", 44),
  message("moss", "user", "Is the build still red?", 12),
  message("moss", "assistant", "Checking now. I need your OK to run the build.", 11),
];

// The event log the long poll reads.
let seq = 0;
const log: { seq: number; event: PhoneEvent }[] = [];
const waiters = new Set<() => void>();

function emit(event: PhoneEvent) {
  log.push({ seq: ++seq, event });
  if (log.length > 500) log.shift();
  for (const wake of waiters) wake();
}

function eventsAfter(after: number, wait: number): Promise<MockResponse> {
  const answer = (): MockResponse => {
    const reset = log.length > 0 && log[0].seq > after + 1;
    return { status: 200, body: { seq, epoch, reset, events: reset ? [] : log.filter((e) => e.seq > after).map((e) => e.event) } };
  };
  if (seq > after) return Promise.resolve(answer());
  return new Promise((resolve) => {
    const wake = () => {
      waiters.delete(wake);
      clearTimeout(timer);
      resolve(answer());
    };
    const timer = setTimeout(wake, Math.min(wait, 25) * 1000);
    waiters.add(wake);
  });
}

const REPLIES = [
  "Sure. I looked at it and here is what I found.\n\nEverything is on track, and nothing needs you right now.",
  "Done. I added it to your notes and will remind you tomorrow morning.",
  "Good question. I don't know yet, so I'll check and come back to you here.",
];

function reply(agentId: string) {
  busy.add(agentId);
  emit({ kind: "status", payload: { agentId, busy: true } });
  const out = message(agentId, "assistant", "", 0, "streaming");
  messages.push(out);
  emit({ kind: "message", payload: { ...out } });
  const words = REPLIES[out.id % REPLIES.length].split(/(?<= )/);
  let i = 0;
  const tick = setInterval(() => {
    if (i < words.length) {
      out.text += words[i];
      emit({ kind: "delta", payload: { agentId, messageId: out.id, chunk: words[i++] } });
      return;
    }
    clearInterval(tick);
    out.status = "done";
    out.updatedAt = Date.now();
    emit({ kind: "message", payload: { ...out } });
    busy.delete(agentId);
    emit({ kind: "status", payload: { agentId, busy: false } });
  }, 120);
}

const ok = (body: unknown): MockResponse => ({ status: 200, body });
const fail = (status: number, error: string): MockResponse => ({ status, body: { error } });

export async function handle(method: string, rawUrl: string, auth: string | undefined, body: unknown): Promise<MockResponse> {
  const url = new URL(rawUrl, "http://mock");
  const path = url.pathname;
  const input = (body ?? {}) as Record<string, unknown>;

  if (method === "POST" && path === "/api/pair") {
    if (String(input.code ?? "").toUpperCase() !== "ABCD2345") return fail(400, "That code didn't work. Check it on your Mac.");
    const token = `mock-${crypto.randomUUID()}`;
    tokens.add(token);
    return ok({ token, deviceId: "phone-1", mac: "Studio Mac" });
  }

  const token = auth?.replace(/^Bearer /, "");
  if (!token || !tokens.has(token)) return fail(401, "Not paired.");

  if (method === "GET" && path === "/api/hello") return ok({ mac: "Studio Mac", device: "Phone", version: "0.4.0" });
  if (method === "POST" && path === "/api/unpair") {
    tokens.delete(token);
    return ok({});
  }
  if (method === "GET" && path === "/api/inbox") return ok(inbox);
  if (method === "GET" && path === "/api/drafts") return ok(drafts);
  if (method === "GET" && path === "/api/familiars") return ok({ familiars, busy: [...busy] });
  if (method === "GET" && path === "/api/events") {
    const after = url.searchParams.get("after");
    if (after === null) return ok({ seq, epoch, events: [], reset: false });
    return eventsAfter(Number(after), Number(url.searchParams.get("wait") ?? 25));
  }

  let m = path.match(/^\/api\/inbox\/(answer|dismiss)$/);
  if (method === "POST" && m) {
    const item = inbox.find((i) => i.id === input.id);
    if (!item) return fail(404, "That question was already answered.");
    if (m[1] === "answer" && item.kind !== "question" && !["once", "deny"].includes(String(input.answer))) return fail(400, "Answer once or deny.");
    inbox = inbox.filter((i) => i !== item);
    emit({ kind: "inbox" });
    return ok({});
  }

  m = path.match(/^\/api\/drafts\/([^/]+)\/(send|discard)$/);
  if (method === "POST" && m) {
    const draft = drafts.find((d) => d.id === decodeURIComponent(m![1]));
    if (!draft || draft.state !== "draft") return fail(404, "That draft is already closed.");
    const closed: Draft = {
      ...draft,
      ...(m[2] === "send" ? { to: input.to as string[], subject: input.subject as string | null, body: String(input.body ?? "") } : {}),
      state: m[2] === "send" ? "sent" : "discarded",
      closedAt: Date.now(),
    };
    drafts = drafts.map((d) => (d === draft ? closed : d));
    emit({ kind: "drafts" });
    return ok(closed);
  }

  m = path.match(/^\/api\/familiars\/([^/]+)\/messages$/);
  if (m) {
    const agentId = decodeURIComponent(m[1]);
    if (!familiars.some((f) => f.id === agentId)) return fail(404, "No such familiar.");
    if (method === "GET") return ok(messages.filter((x) => x.agentId === agentId).slice(-Number(url.searchParams.get("limit") ?? 50)));
    if (method === "POST") {
      const text = String(input.text ?? "").trim();
      if (!text) return fail(400, "Write a message first.");
      const sent = message(agentId, "user", text);
      messages.push(sent);
      emit({ kind: "message", payload: sent });
      setTimeout(() => reply(agentId), 600);
      return ok(sent);
    }
  }

  return fail(404, "Not found.");
}
