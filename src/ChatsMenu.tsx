import { createMemo, createSignal, For, onCleanup, onSettled, Show } from "solid-js";
import { relativeTime, type ChatInfo } from "./api";
import { CheckIcon, ComposeIcon, EditIcon, SearchIcon, TrashIcon } from "./icons";
import "./chats.css";

export const chatTitle = (chat: ChatInfo | undefined) => chat?.title || "New chat";

/** The chat header's list of a familiar's chats: filter, open, rename, delete, or start a new one. */
export default function ChatsMenu(props: {
  chats: ChatInfo[];
  busy: boolean;
  onOpen: (chat: ChatInfo) => void;
  onNew: () => void;
  onRename: (chat: ChatInfo, title: string) => Promise<void>;
  onDelete: (chat: ChatInfo) => Promise<void>;
  onClose: () => void;
}) {
  const [filter, setFilter] = createSignal("");
  const [renaming, setRenaming] = createSignal<string | null>(null);
  const [confirming, setConfirming] = createSignal<string | null>(null);
  let panel!: HTMLDivElement;
  let confirmTimer: number | undefined;
  onCleanup(() => clearTimeout(confirmTimer));

  // Every word typed must appear in the title; the list keeps its most-recent-first order.
  const shown = createMemo(() => {
    const words = filter().toLowerCase().split(/\s+/).filter(Boolean);
    return props.chats.filter((chat) => words.every((word) => chatTitle(chat).toLowerCase().includes(word)));
  });

  // Arrow keys move between the filter box and the rows; Escape closes before anything under the menu.
  const stops = () => Array.from(panel.querySelectorAll<HTMLElement>("[data-nav]"));
  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      e.preventDefault();
      if (renaming()) setRenaming(null);
      else props.onClose();
      return;
    }
    if ((e.key !== "ArrowDown" && e.key !== "ArrowUp") || renaming()) return;
    const list = stops();
    const at = list.indexOf(document.activeElement as HTMLElement);
    const next = e.key === "ArrowDown" ? Math.min(at + 1, list.length - 1) : Math.max(at - 1, 0);
    e.preventDefault();
    list[next]?.focus();
  };
  const onPointer = (e: PointerEvent) => {
    if (!panel.contains(e.target as Node) && !(e.target as Element).closest?.("[data-chats-trigger]")) props.onClose();
  };
  onSettled(() => {
    document.addEventListener("keydown", onKey, true);
    document.addEventListener("pointerdown", onPointer);
    panel.querySelector<HTMLInputElement>("input")?.focus();
    return () => {
      document.removeEventListener("keydown", onKey, true);
      document.removeEventListener("pointerdown", onPointer);
    };
  });

  const askDelete = (chat: ChatInfo) => {
    if (confirming() === chat.id) {
      clearTimeout(confirmTimer);
      setConfirming(null);
      void props.onDelete(chat);
      return;
    }
    setConfirming(chat.id);
    clearTimeout(confirmTimer);
    confirmTimer = window.setTimeout(() => setConfirming(null), 4000);
  };

  return (
    <div class="picker chats-menu" ref={panel} role="dialog" aria-label="Chats">
      <label class="chats-filter">
        <SearchIcon />
        <input
          data-nav
          placeholder="Filter chats"
          aria-label="Filter chats"
          value={filter()}
          onInput={(e) => setFilter(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.isComposing && shown()[0]) props.onOpen(shown()[0]);
          }}
        />
      </label>
      <button type="button" class="picker-item chats-new" data-nav disabled={props.busy} title={props.busy ? "Wait for the reply to finish" : undefined} onClick={props.onNew}>
        <ComposeIcon />
        <span class="picker-name">New chat</span>
      </button>
      <div class="picker-section">Chats</div>
      <div class="picker-list" role="list">
        <Show when={shown().length} fallback={<div class="picker-status">No chats match.</div>}>
          <For each={shown()}>
            {(chat) => (
              <div class={["chats-row", { current: chat.current }]} role="listitem">
                <Show
                  when={renaming() === chat.id}
                  fallback={
                    <>
                      <button
                        type="button"
                        class="picker-item"
                        data-nav
                        aria-current={chat.current ? "true" : undefined}
                        disabled={props.busy && !chat.current}
                        onClick={() => props.onOpen(chat)}
                      >
                        <span class="picker-text">
                          <span class="picker-name">{chatTitle(chat)}</span>
                          <span class="picker-note">{relativeTime(chat.updatedAt)}</span>
                        </span>
                        <Show when={chat.current}>
                          <CheckIcon />
                        </Show>
                      </button>
                      <span class="chats-actions">
                        <button type="button" class="mini-button" aria-label={`Rename ${chatTitle(chat)}`} title="Rename" onClick={() => setRenaming(chat.id)}>
                          <EditIcon />
                        </button>
                        <button
                          type="button"
                          class={["mini-button", { "chats-confirm": confirming() === chat.id }]}
                          aria-label={confirming() === chat.id ? "Click again to delete" : `Delete ${chatTitle(chat)}`}
                          title={confirming() === chat.id ? "Click again to delete" : "Delete"}
                          disabled={props.busy && chat.current}
                          onClick={() => askDelete(chat)}
                        >
                          <TrashIcon />
                          <Show when={confirming() === chat.id}>
                            <span>Delete?</span>
                          </Show>
                        </button>
                      </span>
                    </>
                  }
                >
                  <input
                    class="chats-rename"
                    aria-label="Chat title"
                    value={chat.title}
                    maxlength={80}
                    ref={(el) =>
                      queueMicrotask(() => {
                        el.focus();
                        el.select();
                      })
                    }
                    onKeyDown={(e) => {
                      if (e.key === "Enter" && !e.isComposing) {
                        const title = e.currentTarget.value;
                        void props.onRename(chat, title).then(() => setRenaming(null));
                      }
                    }}
                    onBlur={() => setRenaming(null)}
                  />
                </Show>
              </div>
            )}
          </For>
        </Show>
      </div>
    </div>
  );
}
