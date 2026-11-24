import { createEffect, createSignal, For, Show } from "solid-js";
import { api, errorText, type Familiar, type Message } from "./api";
import { FamiliarFace } from "./Face";

export function ChatList(props: { familiars: Familiar[]; busy: string[]; onOpen: (id: string) => void }) {
  return (
    <section class="page" aria-label="Chat">
      <h1 class="page-title">Chat</h1>
      <Show when={props.familiars.length === 0}>
        <div class="empty">
          <p class="empty-title">No familiars yet</p>
          <p>Make one on your Mac and it shows up here.</p>
        </div>
      </Show>
      <ul class="familiars">
        <For each={props.familiars}>
          {(f) => (
            <li>
              <button class="familiar-row" onClick={() => props.onOpen(f.id)} style={{ "--agent": f.avatar.color }}>
                <FamiliarFace familiar={f} size={40} working={props.busy.includes(f.id)} />
                <span class="familiar-text">
                  <span class="familiar-name">{f.name}</span>
                  <span class="familiar-meta">{props.busy.includes(f.id) ? "Working…" : f.model}</span>
                </span>
                <span class="chevron" aria-hidden="true">
                  ›
                </span>
              </button>
            </li>
          )}
        </For>
      </ul>
    </section>
  );
}

const VISIBLE_ROLES = new Set<Message["role"]>(["user", "assistant", "error"]);

export function Conversation(props: { familiar: Familiar; messages: Message[]; busy: boolean; onBack: () => void; onSent: (m: Message) => void }) {
  const [text, setText] = createSignal("");
  const [sending, setSending] = createSignal(false);
  const [error, setError] = createSignal("");
  let list!: HTMLDivElement;
  let stick = true;
  const shown = () => props.messages.filter((m) => VISIBLE_ROLES.has(m.role) && (m.text || m.status === "streaming"));

  // Stay at the bottom as replies stream in, unless the person scrolled up to read.
  createEffect(
    () => {
      const last = shown().at(-1);
      return `${shown().length}:${last?.text.length ?? 0}`;
    },
    () => {
      if (stick) requestAnimationFrame(() => list && (list.scrollTop = list.scrollHeight));
    },
  );

  const send = async () => {
    const body = text().trim();
    if (!body || sending()) return;
    setSending(true);
    setError("");
    try {
      props.onSent(await api.post(props.familiar.id, body));
      setText("");
      stick = true;
    } catch (e) {
      setError(errorText(e));
    } finally {
      setSending(false);
    }
  };

  const fine = window.matchMedia("(pointer: fine)").matches;

  return (
    <section class="conversation" aria-label={`Chat with ${props.familiar.name}`} style={{ "--agent": props.familiar.avatar.color }}>
      <div class="conversation-head">
        <button class="icon-button" aria-label="All familiars" onClick={props.onBack}>
          <svg width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M12.5 4.5 7 10l5.5 5.5" />
          </svg>
        </button>
        <FamiliarFace familiar={props.familiar} size={28} working={props.busy} />
        <div class="conversation-title">
          <h1>{props.familiar.name}</h1>
          <p>{props.busy ? "Working…" : props.familiar.model}</p>
        </div>
      </div>
      <div
        class="messages"
        ref={list}
        onScroll={() => {
          stick = list.scrollHeight - list.scrollTop - list.clientHeight < 60;
        }}
      >
        <Show when={shown().length === 0}>
          <p class="messages-empty">Say hello to {props.familiar.name}.</p>
        </Show>
        <For each={shown()}>
          {(m) => (
            <div class={["bubble", m.role, { streaming: m.status === "streaming" }]}>
              {m.text || "…"}
            </div>
          )}
        </For>
      </div>
      <Show when={error()}>
        <p class="error-box composer-error" role="alert">
          {error()}
        </p>
      </Show>
      <form
        class="composer"
        onSubmit={(e) => {
          e.preventDefault();
          void send();
        }}
      >
        <textarea
          rows={1}
          aria-label={`Message ${props.familiar.name}`}
          placeholder={`Message ${props.familiar.name}`}
          value={text()}
          onInput={(e) => {
            setText(e.currentTarget.value);
            e.currentTarget.style.height = "auto";
            e.currentTarget.style.height = `${Math.min(e.currentTarget.scrollHeight, 160)}px`;
          }}
          onKeyDown={(e) => {
            if (fine && e.key === "Enter" && !e.shiftKey && !e.isComposing) {
              e.preventDefault();
              void send();
            }
          }}
        />
        <button type="submit" class="primary send" aria-label="Send" disabled={!text().trim() || sending()}>
          <svg width="18" height="18" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M10 16V4M5 9l5-5 5 5" />
          </svg>
        </button>
      </form>
    </section>
  );
}
