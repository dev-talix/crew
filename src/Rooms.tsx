import { createMemo, createSignal, flush, For, Match, Show, Switch } from "solid-js";
import BotAvatar from "./BotAvatar";
import {
  api,
  errorText,
  onEvent,
  onMountAsync,
  relativeTime,
  type Agent,
  type Room,
  type RoomDeltaEvent,
  type RoomFloorEvent,
  type RoomMember,
  type RoomMessage,
  type RoomMessageEvent,
} from "./api";
import { inkFor } from "./avatar";
import { PlusIcon, RoomIcon, SendIcon, StopIcon, TrashIcon } from "./icons";
import { openLinks, renderMarkdown } from "./markdown";
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
import { classifyTool } from "./timeline";
import { EmptyState, Sheet } from "./ui";
import "./chat.css";
import "./rooms.css";

/** Rooms: shared threads where the person and several familiars talk, one familiar at a time. */
export default function RoomsPage(props: { agents: Agent[]; onOpenAgent: (id: string) => void }) {
  const [rooms, setRooms] = createSignal<Room[]>([]);
  const [loaded, setLoaded] = createSignal(false);
  const [openId, setOpenId] = createSignal<string | null>(null);
  const [creating, setCreating] = createSignal(false);
  const reload = () =>
    api.listRooms().then(
      (list) => {
        setRooms(list);
        setLoaded(true);
        if (!list.some((r) => r.id === openId())) setOpenId(list[0]?.id ?? null);
      },
      () => setLoaded(true),
    );
  onMountAsync(async (keep) => {
    for (const name of ["rooms_changed", "agents_changed"] as const) {
      keep(await onEvent(name, () => void reload()));
    }
    await reload();
  });
  const agentById = (id: string | null) => props.agents.find((a) => a.id === id);
  const open = () => rooms().find((r) => r.id === openId());

  return (
    <section class="rooms" data-slot="page">
      <aside class="room-list" aria-label="Rooms">
        <header class="room-list-head" data-tauri-drag-region>
          <h1 data-tauri-drag-region>Rooms</h1>
          <button class="icon-button" aria-label="New room" title="New room" onClick={() => setCreating(true)}>
            <PlusIcon />
          </button>
        </header>
        <div class="room-list-body">
          <For each={rooms()}>
            {(room) => (
              <button
                class={["room-row", { active: room.id === openId() }]}
                aria-current={room.id === openId() ? "page" : undefined}
                onClick={() => setOpenId(room.id)}
              >
                <span class="room-row-title">{room.title}</span>
                <span class="room-row-meta">
                  <span class="avatar-stack" aria-hidden="true">
                    <For each={room.members.slice(0, 4)}>
                      {(m) => <Show when={agentById(m.agentId)}>{(a) => <BotAvatar avatar={a().avatar} size={18} />}</Show>}
                    </For>
                  </span>
                  <span>{relativeTime(room.updatedAt)}</span>
                </span>
              </button>
            )}
          </For>
        </div>
      </aside>
      <div class="room-main">
        <Switch>
          <Match when={open()}>
            {(room) => (
              <Show when={room().id} keyed>
                {(_id) => <RoomThread room={room()} agents={props.agents} onOpenAgent={props.onOpenAgent} />}
              </Show>
            )}
          </Match>
          <Match when={loaded()}>
            <EmptyState
              art={<span class="room-empty-art"><RoomIcon /></span>}
              title="No rooms yet"
              body="A room is one thread with you and several familiars. Mention one as @Name to ask it, and a familiar can hand the floor to another the same way."
              action={
                <button class="primary" disabled={props.agents.length < 2} onClick={() => setCreating(true)}>
                  New room
                </button>
              }
            />
          </Match>
        </Switch>
      </div>
      <Show when={creating()}>
        <NewRoom
          agents={props.agents}
          onClose={() => setCreating(false)}
          onCreated={(room) => {
            setCreating(false);
            setRooms((list) => [room, ...list.filter((r) => r.id !== room.id)]);
            setOpenId(room.id);
          }}
        />
      </Show>
    </section>
  );
}

function NewRoom(props: { agents: Agent[]; onClose: () => void; onCreated: (room: Room) => void }) {
  const [title, setTitle] = createSignal("");
  const [picked, setPicked] = createSignal<string[]>([]);
  const [error, setError] = createSignal("");
  const [saving, setSaving] = createSignal(false);
  const toggle = (id: string) => setPicked((list) => (list.includes(id) ? list.filter((x) => x !== id) : [...list, id]));
  const create = async () => {
    setSaving(true);
    setError("");
    try {
      props.onCreated(await api.createRoom(title(), picked()));
    } catch (e) {
      setError(errorText(e));
    } finally {
      setSaving(false);
    }
  };
  return (
    <Sheet label="New room" onClose={props.onClose}>
      <form
        class="room-form wf-form"
        onSubmit={(e) => {
          e.preventDefault();
          void create();
        }}
      >
        <h1>New room</h1>
        <label class="field">
          <span>Name</span>
          <input value={title()} maxlength={80} placeholder="Named after its familiars if you leave it empty" onInput={(e) => setTitle(e.currentTarget.value)} />
        </label>
        <div class="field">
          <span>Familiars</span>
          <div class="coven-members">
            <For each={props.agents}>
              {(agent) => {
                const order = () => picked().indexOf(agent.id);
                return (
                  <label class={["coven-member", { on: order() >= 0 }]}>
                    <input type="checkbox" checked={order() >= 0} onChange={() => toggle(agent.id)} />
                    <BotAvatar avatar={agent.avatar} size={22} alive={false} />
                    <span>{agent.name}</span>
                    <Show when={order() >= 0}>
                      <span class="room-pick-order">{order() + 1}</span>
                    </Show>
                  </label>
                );
              }}
            </For>
          </div>
          <p class="hint">Pick 2 to 8. The first one you pick answers a message that mentions nobody, until a familiar has replied.</p>
        </div>
        <Show when={error()}>
          <p class="error-box" role="alert">
            {error()}
          </p>
        </Show>
        <div class="form-actions">
          <span class="spacer" />
          <button type="button" class="ghost" onClick={props.onClose}>
            Cancel
          </button>
          <button type="submit" class="primary" disabled={saving() || picked().length < 2}>
            Create room
          </button>
        </div>
      </form>
    </Sheet>
  );
}

function RoomThread(props: { room: Room; agents: Agent[]; onOpenAgent: (id: string) => void }) {
  const [messages, setMessages] = createSignal<RoomMessage[]>([]);
  const [speaking, setSpeaking] = createSignal<string | null>(null);
  const [queue, setQueue] = createSignal<string[]>([]);
  const [draft, setDraft] = createSignal("");
  const [caret, setCaret] = createSignal(0);
  const [picked, setPicked] = createSignal(0);
  const [error, setError] = createSignal("");
  const [gone, setGone] = createSignal(false);
  const [hasOlder, setHasOlder] = createSignal(false);
  let log: HTMLDivElement | undefined;
  let input: HTMLTextAreaElement | undefined;
  const roomId = props.room.id;
  const members = () => props.room.members;
  const agentById = (id: string | null) => props.agents.find((a) => a.id === id);
  const nameOf = (id: string) => members().find((m) => m.agentId === id)?.name ?? agentById(id)?.name ?? "A familiar";

  const atBottom = () => !log || log.scrollHeight - log.scrollTop - log.clientHeight < 80;
  const follow = (update: () => void) => {
    const stick = atBottom();
    update();
    if (stick && log) {
      flush();
      log.scrollTop = log.scrollHeight;
    }
  };

  onMountAsync(async (keep) => {
    keep(
      await onEvent<RoomMessageEvent>("room_message", (e) => {
        if (e.roomId === roomId) follow(() => setMessages((list) => upsertRoomMessage(list, e.message)));
      }),
    );
    keep(
      await onEvent<RoomDeltaEvent>("room_delta", (e) => {
        if (e.roomId === roomId) follow(() => setMessages((list) => appendRoomChunk(list, e.messageId, e.chunk)));
      }),
    );
    keep(
      await onEvent<RoomFloorEvent>("room_floor", (e) => {
        if (e.roomId !== roomId) return;
        setSpeaking(e.speaking);
        setQueue(e.queue);
      }),
    );
    try {
      const thread = await api.roomThread(roomId);
      follow(() => setMessages((live) => mergeRoomSnapshot(thread.messages, live)));
      setSpeaking(thread.speaking);
      setQueue(thread.queue);
      setHasOlder(thread.hasOlder);
      if (log) log.scrollTop = log.scrollHeight;
    } catch {
      setGone(true);
    }
  });

  const query = createMemo(() => mentionQuery(draft().slice(0, caret())));
  const suggestions = createMemo(() => {
    const q = query();
    return q ? mentionMatches(q.query, members()) : [];
  });
  const insertMention = (name: string) => {
    const next = completeMention(draft(), input ? input.selectionStart : draft().length, name);
    setDraft(next.text);
    setCaret(next.caret);
    flush();
    input?.focus();
    input?.setSelectionRange(next.caret, next.caret);
  };

  const loadOlder = async () => {
    const first = messages()[0];
    if (!first) return;
    try {
      const page = await api.roomHistory(roomId, first.id);
      const height = log?.scrollHeight ?? 0;
      setMessages((list) => mergeRoomSnapshot(page.messages, list));
      setHasOlder(page.hasOlder);
      flush();
      if (log) log.scrollTop += log.scrollHeight - height;
    } catch (e) {
      setError(errorText(e));
    }
  };

  const send = async () => {
    const text = draft().trim();
    if (!text) return;
    setError("");
    try {
      await api.roomSend(roomId, text);
      setDraft("");
      setCaret(0);
      if (log) log.scrollTop = log.scrollHeight;
    } catch (e) {
      setError(errorText(e));
    }
  };
  const stop = () => api.roomStop(roomId).catch((e) => setError(errorText(e)));
  const remove = async () => {
    if (!window.confirm(`Delete "${props.room.title}" and its whole history?`)) return;
    await api.deleteRoom(roomId).catch((e) => setError(errorText(e)));
  };

  return (
    <div class="room-thread">
      <header class="room-head" data-tauri-drag-region>
        <div class="room-head-text" data-tauri-drag-region>
          <h2 data-tauri-drag-region>{props.room.title}</h2>
          <span class="room-head-count">{members().length} familiars</span>
        </div>
        <div class="room-members" role="list" aria-label="Familiars in this room">
          <For each={members()}>
            {(m) => {
              const agent = () => agentById(m.agentId);
              const state = () => (speaking() === m.agentId ? "speaking" : queue().includes(m.agentId) ? "waiting" : "");
              return (
                <button
                  role="listitem"
                  class={["room-member", state()]}
                  title={state() === "speaking" ? `${m.name} has the floor` : state() === "waiting" ? `${m.name} is waiting for the floor` : `Mention ${m.name}`}
                  onClick={() => insertMention(m.name)}
                >
                  <Show when={agent()}>{(a) => <BotAvatar avatar={a().avatar} size={24} working={state() === "speaking"} />}</Show>
                  <span>{m.name}</span>
                </button>
              );
            }}
          </For>
        </div>
        <button class="icon-button" aria-label="Delete room" title="Delete room" onClick={remove}>
          <TrashIcon />
        </button>
      </header>
      <div class="timeline" ref={log} role="log" aria-label={`Messages in ${props.room.title}`}>
        <div class="timeline-column">
          <Show when={gone()}>
            <p class="room-note">This room was deleted.</p>
          </Show>
          <Show when={hasOlder()}>
            <button class="mini-link load-older" onClick={() => void loadOlder()}>
              Load earlier messages
            </button>
          </Show>
          <Show when={!gone() && messages().length === 0}>
            <div class="chat-empty">
              <span class="avatar-stack big" aria-hidden="true">
                <For each={members()}>{(m) => <Show when={agentById(m.agentId)}>{(a) => <BotAvatar avatar={a().avatar} size={44} />}</Show>}</For>
              </span>
              <p>Say something to the room, or mention a familiar with @ to ask it first.</p>
            </div>
          </Show>
          <For each={messages()}>{(m) => <RoomRow message={m} members={members()} agent={agentById(m.agentId)} target={agentById(m.targetId)} />}</For>
        </div>
      </div>
      <footer class="composer-dock" data-slot="composer">
        <Show when={error()}>
          <p class="error-box composer-error" role="alert">
            {error()}
          </p>
        </Show>
        <p class="room-floor" role="status">
          <Show when={speaking()}>
            <span class="pulse" aria-hidden="true">
              <i />
              <i />
              <i />
            </span>
            <span>{floorLine(speaking(), queue(), nameOf)}</span>
          </Show>
        </p>
        <div class="composer-card">
          <Show when={suggestions().length > 0}>
            <ul class="mention-menu" role="listbox" aria-label="Mention a familiar">
              <For each={suggestions()}>
                {(m, i) => (
                  <li
                    role="option"
                    aria-selected={i() === picked() % suggestions().length ? "true" : "false"}
                    class={{ on: i() === picked() % suggestions().length }}
                    onMouseDown={(e) => {
                      e.preventDefault();
                      insertMention(m.name);
                    }}
                  >
                    <Show when={agentById(m.agentId)}>{(a) => <BotAvatar avatar={a().avatar} size={20} />}</Show>
                    <span>{m.name}</span>
                  </li>
                )}
              </For>
            </ul>
          </Show>
          <textarea
            ref={input}
            rows={1}
            aria-label={`Message ${props.room.title}`}
            placeholder="Type @ to mention a familiar"
            value={draft()}
            onInput={(e) => {
              setDraft(e.currentTarget.value);
              setCaret(e.currentTarget.selectionStart);
              setPicked(0);
            }}
            onSelect={(e) => setCaret(e.currentTarget.selectionStart)}
            onKeyDown={(e) => {
              const list = suggestions();
              if (list.length && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
                e.preventDefault();
                setPicked((n) => n + (e.key === "ArrowDown" ? 1 : list.length - 1));
                return;
              }
              if (list.length && (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey && !e.isComposing))) {
                e.preventDefault();
                insertMention(list[picked() % list.length].name);
                return;
              }
              if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
                e.preventDefault();
                void send();
              }
            }}
          />
          <div class="composer-footer">
            <span class="room-hint">One familiar replies at a time. A familiar hands off by mentioning another.</span>
            <span class="spacer" />
            <Show when={speaking() && !draft().trim()}>
              <button class="round-button stop" aria-label="Stop" title="Stop the room" onClick={stop}>
                <StopIcon />
              </button>
            </Show>
            <Show when={!speaking() || draft().trim()}>
              <button class="round-button send" aria-label="Send" title="Send" disabled={!draft().trim()} onClick={() => void send()}>
                <SendIcon />
              </button>
            </Show>
          </div>
        </div>
      </footer>
    </div>
  );
}

function Mentions(props: { text: string; members: RoomMember[] }) {
  return (
    <For each={mentionSegments(props.text, props.members)}>
      {(s) => (s.memberId ? <span class="mention">{s.text}</span> : s.text)}
    </For>
  );
}

function RoomRow(props: { message: RoomMessage; members: RoomMember[]; agent?: Agent; target?: Agent }) {
  const m = () => props.message;
  const name = () => m().name ?? "A familiar who left";
  const color = () => props.agent?.avatar.color ?? "var(--muted)";
  return (
    <Switch>
      <Match when={m().kind === "user"}>
        <div class="row user" data-slot="message" data-role="user" data-message-id={m().id}>
          <div class="user-bubble">
            <Mentions text={m().text} members={props.members} />
          </div>
        </div>
      </Match>
      <Match when={m().kind === "reply"}>
        <ReplyRow message={m()} agent={props.agent} name={name()} />
      </Match>
      <Match when={m().kind === "tool"}>
        <p class="room-tool" data-message-id={m().id}>
          <span class="room-tool-who" style={{ color: color() }}>
            {name()}
          </span>{" "}
          {classifyTool(m().text).label}
        </p>
      </Match>
      <Match when={m().kind === "pass"}>
        <div class="room-pass" role="note" data-message-id={m().id}>
          <span class="room-pass-line" aria-hidden="true" />
          <Show when={props.agent}>{(a) => <BotAvatar avatar={a().avatar} size={18} />}</Show>
          <span>
            <b>{name()}</b> handed off to <b>{m().targetName ?? "a familiar who left"}</b>
          </span>
          <Show when={props.target}>{(a) => <BotAvatar avatar={a().avatar} size={18} />}</Show>
          <span class="room-pass-line" aria-hidden="true" />
        </div>
      </Match>
      <Match when={m().kind === "note"}>
        <p class="room-note" role="note" data-message-id={m().id}>
          {m().text}
        </p>
      </Match>
      <Match when={m().kind === "error"}>
        <div class="row error-box" data-message-id={m().id}>
          <b>{name()}</b> couldn't reply. {m().text}
        </div>
      </Match>
    </Switch>
  );
}

function ReplyRow(props: { message: RoomMessage; agent?: Agent; name: string }) {
  const m = () => props.message;
  const streaming = () => m().status === "streaming";
  const html = createMemo(() => (streaming() ? "" : renderMarkdown(m().text)));
  const [prompt, setPrompt] = createSignal<string | null>(null);
  const [showing, setShowing] = createSignal(false);
  const color = () => props.agent?.avatar.color ?? "#807e8a";
  const togglePrompt = async () => {
    if (!showing() && prompt() === null) {
      setPrompt(await api.roomPrompt(m().id).catch((e) => errorText(e)));
    }
    setShowing(!showing());
  };
  const note = () => (m().status === "stopped" ? "Stopped" : m().status === "interrupted" ? "Cut off when Grimoire closed" : null);
  return (
    <div
      class="row assistant room-reply"
      data-slot="message"
      data-role="assistant"
      data-message-id={m().id}
      style={{ "--agent": color(), "--agent-ink": inkFor(color()) }}
    >
      <div class="room-reply-head">
        <Show when={props.agent} fallback={<span class="room-avatar-gone" />}>
          {(a) => <BotAvatar avatar={a().avatar} size={22} working={streaming()} />}
        </Show>
        <span class="room-reply-name">{props.name}</span>
        <span class="room-reply-time">{new Date(m().createdAt).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}</span>
        <Show when={note()}>{(n) => <span class="room-reply-time">{n()}</span>}</Show>
      </div>
      <div class="room-reply-body">
        <Show when={!streaming()} fallback={<div class="plain">{m().text.trimEnd()}<span class="caret" aria-hidden="true" /></div>}>
          <div class="md" innerHTML={html()} onClick={openLinks} />
        </Show>
        <Show when={m().hasPrompt && !streaming()}>
          <button class="mini-link room-prompt-toggle" aria-expanded={showing() ? "true" : "false"} onClick={() => void togglePrompt()}>
            {showing() ? "Hide what it was sent" : `What ${props.name} was sent`}
          </button>
          <Show when={showing()}>
            <pre class="room-prompt">{prompt()}</pre>
          </Show>
        </Show>
      </div>
    </div>
  );
}
