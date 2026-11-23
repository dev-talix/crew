import { createMemo, createSignal, For, Match, onSettled, Show, Switch } from "solid-js";
import { api, onEvent, onMountAsync, type Message, type SideDeltaEvent, type SideMessageEvent } from "./api";
import { ChevronIcon, TalkIcon } from "./icons";
import { isRemoteThread } from "./linked";
import { openLinks, renderMarkdown } from "./markdown";
import { appendChunk, mergeSnapshot, partnerName, sideState, upsert } from "./side";
import { classifyTool } from "./timeline";

const STATE_NOTES = { working: "working", replied: "replied", failed: "couldn't reply", empty: "" } as const;

/**
 * A familiar talking to another behind the scenes. The row sits in the asking familiar's chat and opens
 * the side conversation in place, live while the other familiar works.
 */
export default function SideConversation(props: { message: Message; asker: string; startOpen?: boolean }) {
  // A conversation with a familiar on a linked Mac lives on that Mac, so the row only names it.
  if (isRemoteThread(props.message.source)) {
    return (
      <div class="row side" data-slot="message" data-role="side" data-message-id={props.message.id}>
        <div class="tool-line">
          <TalkIcon />
          <span>{props.message.text}</span>
        </div>
      </div>
    );
  }
  return <LocalConversation {...props} />;
}

function LocalConversation(props: { message: Message; asker: string; startOpen?: boolean }) {
  const chatId = () => props.message.source ?? "";
  const partner = () => partnerName(props.message.text);
  const [open, setOpen] = createSignal(props.startOpen ?? false);
  const [messages, setMessages] = createSignal<Message[]>([]);
  const [gone, setGone] = createSignal(false);
  const state = createMemo(() => sideState(messages()));

  onSettled(() => {
    api.sideThread(chatId()).then(
      // Messages may already have arrived live while this loaded; keep the newer copy of each.
      (thread) => setMessages((live) => mergeSnapshot(thread.messages, live)),
      () => setGone(true),
    );
  });

  onMountAsync(async (keep) => {
    keep(
      await onEvent<SideMessageEvent>("side_message", (e) => {
        if (e.chatId === chatId()) setMessages((list) => upsert(list, e.message));
      }),
    );
    keep(
      await onEvent<SideDeltaEvent>("side_delta", (e) => {
        if (e.chatId === chatId()) setMessages((list) => appendChunk(list, e.messageId, e.chunk));
      }),
    );
  });

  return (
    <div class="row side" data-slot="message" data-role="side" data-message-id={props.message.id}>
      <button
        class="tool-line tool-toggle side-toggle"
        aria-expanded={open() ? "true" : "false"}
        title={`Open the conversation with ${partner()}`}
        onClick={() => setOpen(!open())}
      >
        <TalkIcon />
        <span>{props.message.text}</span>
        <Show when={STATE_NOTES[state()]}>
          {(note) => <span class={["side-state", state()]}>{note()}</span>}
        </Show>
        <span class={["chevron", { open: open() }]}>
          <ChevronIcon />
        </span>
      </button>
      <Show when={open()}>
        <div class="side-thread" role="log" aria-label={`Conversation between ${props.asker} and ${partner()}`}>
          <Show when={!gone()} fallback={<p class="side-note">This conversation was deleted.</p>}>
            <For each={messages()}>
              {(m) => (
                <Switch>
                  <Match when={m.role === "user"}>
                    <div class="side-msg">
                      <div class="side-who">{m.source?.replace(/^From /, "") || props.asker}</div>
                      <div class="side-text">{m.text}</div>
                    </div>
                  </Match>
                  <Match when={m.role === "assistant"}>
                    <div class="side-msg">
                      <div class="side-who">{partner()}</div>
                      <Show when={m.status !== "streaming"} fallback={<div class="side-text">{m.text}</div>}>
                        <div class="md side-md" innerHTML={renderMarkdown(m.text)} onClick={openLinks} />
                      </Show>
                    </div>
                  </Match>
                  <Match when={m.role === "error"}>
                    <p class="side-note error">{m.text}</p>
                  </Match>
                  <Match when={m.role === "tool"}>
                    <p class="side-note">{classifyTool(m.text).label}</p>
                  </Match>
                  <Match when={m.role === "side"}>
                    {/* The other familiar asked someone else while answering. */}
                    <SideConversation message={m} asker={partner()} />
                  </Match>
                </Switch>
              )}
            </For>
            <Show when={state() === "working"}>
              <p class="side-note working">{partner()} is working…</p>
            </Show>
          </Show>
        </div>
      </Show>
    </div>
  );
}
