import type { Draft, DraftEdits, InboxItem, Message } from "../api";
import type { Avatar } from "../avatar";
import type { PhoneEvent } from "./logic";

export type { Draft, InboxItem, Message };

export interface Familiar {
  id: string;
  name: string;
  provider: string;
  model: string;
  avatar: Avatar;
}

export interface EventBatch {
  seq: number;
  epoch: string;
  events: PhoneEvent[];
  reset: boolean;
}

const TOKEN_KEY = "grimoire.phone.token";

function storage(): Storage | null {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

export const token = () => storage()?.getItem(TOKEN_KEY) ?? null;
export const saveToken = (value: string) => storage()?.setItem(TOKEN_KEY, value);
export const clearToken = () => storage()?.removeItem(TOKEN_KEY);

/** Raised for a 401, after the token is cleared, so the app can go back to pairing. */
export class Unauthorized extends Error {}

let onUnauthorized: () => void = () => {};
export const whenUnauthorized = (fn: () => void) => (onUnauthorized = fn);

async function call<T>(method: "GET" | "POST", path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
  const headers: Record<string, string> = {};
  const saved = token();
  if (saved) headers.Authorization = `Bearer ${saved}`;
  if (body !== undefined) headers["Content-Type"] = "application/json";
  const res = await fetch(path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body), signal });
  if (res.status === 401 && path !== "/api/pair") {
    clearToken();
    onUnauthorized();
    throw new Unauthorized("This phone is no longer paired.");
  }
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(typeof data?.error === "string" ? data.error : `Request failed (${res.status}).`);
  return data as T;
}

const id = encodeURIComponent;

export const api = {
  pair: (code: string, name: string) => call<{ token: string; deviceId: string; mac: string }>("POST", "/api/pair", { code, name }),
  hello: () => call<{ mac: string; device: string; version: string }>("GET", "/api/hello"),
  unpair: () => call<object>("POST", "/api/unpair"),
  inbox: () => call<InboxItem[]>("GET", "/api/inbox"),
  // Ids go in the body: approval ids come from the CLIs and may hold any character.
  answer: (item: InboxItem, answer: string) => call<object>("POST", "/api/inbox/answer", { id: item.id, kind: item.kind, answer }),
  dismiss: (item: InboxItem) => call<object>("POST", "/api/inbox/dismiss", { id: item.id }),
  drafts: () => call<Draft[]>("GET", "/api/drafts"),
  send: (draftId: string, edits: DraftEdits) => call<Draft>("POST", `/api/drafts/${id(draftId)}/send`, edits),
  discard: (draftId: string) => call<Draft>("POST", `/api/drafts/${id(draftId)}/discard`),
  familiars: () => call<{ familiars: Familiar[]; busy: string[] }>("GET", "/api/familiars"),
  messages: (agentId: string) => call<Message[]>("GET", `/api/familiars/${id(agentId)}/messages?limit=50`),
  post: (agentId: string, text: string) => call<Message>("POST", `/api/familiars/${id(agentId)}/messages`, { text }),
  events: (after: number | null, signal?: AbortSignal) =>
    call<EventBatch>("GET", after === null ? "/api/events" : `/api/events?after=${after}&wait=25`, undefined, signal),
};

export function errorText(error: unknown): string {
  return error instanceof Error ? error.message : typeof error === "string" ? error : "Something went wrong.";
}
