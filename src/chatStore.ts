import { createEffect, createSignal, on, type Accessor } from "solid-js";
import { createStore } from "solid-js/store";
import { api, PAGE_SIZE, type DeltaEvent, type Message } from "./api";

export function createChatStore(selected: Accessor<string | null>) {
  const [messages, setMessages] = createStore<Message[]>([]);
  const [hasOlder, setHasOlder] = createSignal(false);
  let loading = false;
  let pending: (() => void)[] = [];
  let loadToken = 0;

  const indexOf = (id: number) => {
    for (let i = messages.length - 1; i >= 0; i--) if (messages[i].id === id) return i;
    return -1;
  };

  const upsert = (message: Message) => {
    const index = indexOf(message.id);
    if (index >= 0) return setMessages(index, message);
    const last = messages[messages.length - 1];
    if (!last || message.id > last.id) return setMessages(messages.length, message);
    setMessages((list) => [...list, message].sort((a, b) => a.id - b.id));
  };

  const appendChunk = ({ messageId, chunk }: DeltaEvent) => {
    const index = indexOf(messageId);
    if (index >= 0) setMessages(index, "text", (text) => text + chunk);
  };

  const whenLoaded = (apply: () => void) => (loading ? pending.push(apply) : apply());

  createEffect(
    on(selected, async (agentId) => {
      const token = ++loadToken;
      setMessages([]);
      setHasOlder(false);
      pending = [];
      if (!agentId) return;
      loading = true;
      try {
        const page = await api.listMessages(agentId);
        if (token !== loadToken) return;
        setMessages(page);
        setHasOlder(page.length === PAGE_SIZE);
      } finally {
        if (token === loadToken) {
          loading = false;
          pending.forEach((apply) => apply());
          pending = [];
        }
      }
    }),
  );

  return {
    messages,
    hasOlder,
    onMessage: (message: Message) => {
      if (message.agentId === selected()) whenLoaded(() => upsert(message));
    },
    onDelta: (delta: DeltaEvent) => {
      if (delta.agentId === selected()) whenLoaded(() => appendChunk(delta));
    },
    loadOlder: async () => {
      const agentId = selected();
      const first = messages[0];
      if (!agentId || !first) return;
      const older = await api.listMessages(agentId, first.id);
      if (agentId !== selected()) return;
      setMessages((list) => [...older, ...list]);
      setHasOlder(older.length === PAGE_SIZE);
    },
  };
}

export type ChatStore = ReturnType<typeof createChatStore>;
