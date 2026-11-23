import { createEffect, createSignal, createStore, flush, type Accessor } from "solid-js";
import { api, PAGE_SIZE, type DeltaEvent, type Message } from "./api";

const lastIndexOf = (list: Message[], id: number) => {
  for (let i = list.length - 1; i >= 0; i--) if (list[i].id === id) return i;
  return -1;
};

export function createChatStore(selected: Accessor<string | null>) {
  const [messages, setMessages] = createStore<Message[]>([]);
  const [hasOlder, setHasOlder] = createSignal(false);
  let loading = false;
  let pending: (() => void)[] = [];
  let loadToken = 0;

  // Lookups run inside the draft setter so they see writes that have not flushed yet,
  // like a message that arrived in the same tick as its first delta.
  const upsert = (message: Message) =>
    setMessages((list) => {
      const index = lastIndexOf(list, message.id);
      if (index >= 0) {
        // Merge rather than replace: Chat's row cache holds this item and must see the update.
        Object.assign(list[index], message);
        return;
      }
      const after = list.findIndex((m) => m.id > message.id);
      list.splice(after < 0 ? list.length : after, 0, message);
    });

  const appendChunk = ({ messageId, chunk }: DeltaEvent) =>
    setMessages((list) => {
      const index = lastIndexOf(list, messageId);
      if (index >= 0) list[index].text += chunk;
    });

  const whenLoaded = (apply: () => void) => (loading ? pending.push(apply) : apply());

  const load = async (agentId: string | null) => {
    const token = ++loadToken;
    setMessages(() => []);
    setHasOlder(false);
    pending = [];
    if (!agentId) return;
    loading = true;
    try {
      const page = await api.listMessages(agentId);
      if (token !== loadToken) return;
      setMessages(() => page);
      setHasOlder(page.length === PAGE_SIZE);
    } finally {
      if (token === loadToken) {
        loading = false;
        pending.forEach((apply) => apply());
        pending = [];
      }
    }
  };

  // The latest load, so a jump to a message waits for the page it lands on.
  let loaded: Promise<void> = Promise.resolve();
  const reload = () => (loaded = load(selected()));

  createEffect(selected, () => {
    void reload();
  });

  const loadOlder = async () => {
    const agentId = selected();
    const first = messages[0];
    if (!agentId || !first) return;
    const older = await api.listMessages(agentId, first.id);
    if (agentId !== selected()) return;
    setMessages((list) => {
      list.unshift(...older);
    });
    setHasOlder(older.length === PAGE_SIZE);
  };

  return {
    messages,
    hasOlder,
    onMessage: (message: Message) => {
      if (message.agentId === selected()) whenLoaded(() => upsert(message));
    },
    onDelta: (delta: DeltaEvent) => {
      if (delta.agentId === selected()) whenLoaded(() => appendChunk(delta));
    },
    loadOlder,
    /** Loads the familiar's current chat again, after switching chats. */
    reload,
    /** Loads earlier pages until `messageId` is in the list. Returns whether it is. */
    loadUntil: async (messageId: number) => {
      const agentId = selected();
      for (let waiting = loaded; ; waiting = loaded) {
        await waiting;
        if (waiting === loaded) break;
      }
      flush();
      while (agentId === selected() && hasOlder() && messages.length && messages[0].id > messageId) {
        const before = messages[0].id;
        await loadOlder();
        flush();
        if (messages[0]?.id === before) break;
      }
      return messages.some((m) => m.id === messageId);
    },
  };
}

export type ChatStore = ReturnType<typeof createChatStore>;
