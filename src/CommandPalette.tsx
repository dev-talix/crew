import { createEffect, createMemo, createSignal, For, onSettled, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import BotAvatar from "./BotAvatar";
import { api, relativeTime, type Agent, type SearchHit, type Workflow } from "./api";
import { ClockIcon, ComposeIcon, HomeIcon, LinkIcon, PlusIcon, SearchIcon } from "./icons";
import { highlightParts, rank, snippetParts, type PaletteItem } from "./palette";
import { SETTINGS_SECTIONS, type SettingsSection } from "./SettingsPage";
import "./palette.css";

export interface PaletteActions {
  openAgent: (id: string) => void;
  castSpell: (id: string) => void;
  newSpell: () => void;
  openRituals: () => void;
  openHome: () => void;
  openSettings: (section: SettingsSection) => void;
  summon: () => void;
  newChat: (agentId: string) => void;
  openMessage: (hit: SearchHit) => void;
}

interface Command extends PaletteItem {
  kind: string;
  icon: () => JSX.Element;
  run: () => void;
}

type Option = { id: string; run: () => void };

const SEARCH_DELAY = 150;

function Parts(props: { parts: { text: string; hit: boolean }[] }) {
  return <For each={props.parts}>{(part) => (part.hit ? <mark>{part.text}</mark> : <>{part.text}</>)}</For>;
}

/** ⌘K: jump to a familiar, spell, page, or setting, run an action, or find a message. */
export default function CommandPalette(props: {
  agents: Agent[];
  spells: Workflow[];
  /** The familiar whose chat is open, for "New chat". */
  current: Agent | undefined;
  actions: PaletteActions;
  onClose: () => void;
}) {
  const [query, setQuery] = createSignal("");
  const [hits, setHits] = createSignal<SearchHit[]>([]);
  const [searching, setSearching] = createSignal(false);
  const [active, setActive] = createSignal(0);
  let input!: HTMLInputElement;
  let listbox!: HTMLDivElement;
  const agentById = (id: string) => props.agents.find((a) => a.id === id);

  const commands = createMemo<Command[]>(() => {
    const a = props.actions;
    const list: Command[] = props.agents.map((agent) => ({
      id: `agent:${agent.id}`,
      label: agent.name,
      kind: agent.home ? `Familiar on ${agent.home.name}` : "Familiar",
      icon: () => <BotAvatar avatar={agent.avatar} size={18} alive={false} />,
      run: () => a.openAgent(agent.id),
    }));
    for (const spell of props.spells) {
      list.push({ id: `spell:${spell.id}`, label: spell.name, kind: "Cast spell", icon: SpellGlyph, run: () => a.castSpell(spell.id) });
    }
    list.push(
      { id: "home", label: "Home", kind: "Page", icon: HomeIcon, run: a.openHome },
      { id: "rituals", label: "Rituals", kind: "Page", icon: ClockIcon, keywords: "routines schedules", run: a.openRituals },
    );
    for (const section of SETTINGS_SECTIONS) {
      list.push({
        id: `settings:${section.id}`,
        label: `Settings: ${section.label}`,
        kind: "Settings",
        keywords: section.keywords,
        icon: section.icon,
        run: () => a.openSettings(section.id),
      });
    }
    const current = props.current;
    // A linked Mac's familiar keeps its chats on that Mac.
    if (current && !current.home) {
      list.push({ id: "new-chat", label: `New chat with ${current.name}`, kind: "Action", icon: ComposeIcon, run: () => a.newChat(current.id) });
    }
    list.push(
      { id: "summon", label: "Summon a familiar", kind: "Action", keywords: "new familiar create agent", icon: PlusIcon, run: a.summon },
      { id: "new-spell", label: "New spell", kind: "Action", keywords: "create workflow", icon: PlusIcon, run: a.newSpell },
      { id: "link-mac", label: "Link a Mac", kind: "Action", keywords: "pair tailscale tailnet linked macs", icon: LinkIcon, run: () => a.openSettings("links") },
    );
    return list;
  });

  const ranked = createMemo(() => rank(commands(), query()));
  const options = createMemo<Option[]>(() => [
    ...ranked().map((r) => ({ id: r.item.id, run: r.item.run })),
    ...hits().map((hit) => ({ id: `message:${hit.messageId}`, run: () => props.actions.openMessage(hit) })),
  ]);
  const optionId = (id: string) => `palette-${id.replace(/[^\w-]/g, "-")}`;
  const activeId = () => options()[active()]?.id;

  createEffect(query, (q) => {
    setActive(0);
    const text = q.trim();
    if (text.length < 2) {
      setHits([]);
      setSearching(false);
      return;
    }
    let live = true;
    setSearching(true);
    const timer = setTimeout(() => {
      api.searchMessages(text).then(
        (found) => live && (setHits(found ?? []), setSearching(false)),
        () => live && (setHits([]), setSearching(false)),
      );
    }, SEARCH_DELAY);
    return () => {
      live = false;
      clearTimeout(timer);
    };
  });

  createEffect(activeId, (id) => {
    if (id) listbox.querySelector(`#${optionId(id)}`)?.scrollIntoView({ block: "nearest" });
  });

  const choose = (index: number) => {
    const option = options()[index];
    if (!option) return;
    props.onClose();
    option.run();
  };

  // Keys go to the palette first, so Escape doesn't also close Settings or a sheet underneath.
  const onKey = (e: KeyboardEvent) => {
    const count = options().length;
    if (e.key === "Escape" || (e.key === "k" && e.metaKey)) {
      e.preventDefault();
      e.stopPropagation();
      props.onClose();
    } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      e.stopPropagation();
      if (count) setActive((i) => (i + (e.key === "ArrowDown" ? 1 : count - 1)) % count);
    } else if (e.key === "Enter" && !e.isComposing) {
      e.preventDefault();
      e.stopPropagation();
      choose(active());
    }
  };
  onSettled(() => {
    document.addEventListener("keydown", onKey, true);
    input.focus();
    return () => document.removeEventListener("keydown", onKey, true);
  });

  const Row = (row: { id: string; index: () => number; children: JSX.Element; class?: string }) => (
    <div
      id={optionId(row.id)}
      role="option"
      class={["palette-option", row.class, { active: active() === row.index() }]}
      aria-selected={active() === row.index() ? "true" : "false"}
      onPointerMove={() => active() !== row.index() && setActive(row.index())}
      onClick={() => choose(row.index())}
    >
      {row.children}
    </div>
  );

  return (
    <div class="palette-backdrop" onClick={(e) => e.target === e.currentTarget && props.onClose()}>
      <div class="palette" role="dialog" aria-modal="true" aria-label="Command palette">
        <label class="palette-input">
          <SearchIcon />
          <input
            ref={input}
            role="combobox"
            aria-expanded="true"
            aria-controls="palette-list"
            aria-autocomplete="list"
            aria-activedescendant={activeId() ? optionId(activeId()!) : undefined}
            placeholder="Jump to a familiar, spell, or setting, or search messages"
            value={query()}
            onInput={(e) => setQuery(e.currentTarget.value)}
          />
          <kbd>esc</kbd>
        </label>
        <div class="palette-list" id="palette-list" role="listbox" aria-label="Results" ref={listbox}>
          <Show when={ranked().length}>
            <div class="palette-group" role="presentation">
              {query().trim() ? "Jump to" : "Go to"}
            </div>
            <For each={ranked()}>
              {(entry, index) => (
                <Row id={entry.item.id} index={index}>
                  <span class="palette-icon">{entry.item.icon()}</span>
                  <span class="palette-label">
                    <Parts parts={highlightParts(entry.item.label, entry.indices)} />
                  </span>
                  <span class="palette-kind">{entry.item.kind}</span>
                </Row>
              )}
            </For>
          </Show>
          <Show when={hits().length}>
            <div class="palette-group" role="presentation">
              Messages
            </div>
            <For each={hits()}>
              {(hit, index) => {
                const agent = () => agentById(hit.agentId);
                return (
                  <Row id={`message:${hit.messageId}`} index={() => ranked().length + index()} class="palette-hit">
                    <span class="palette-icon">
                      <Show when={agent()}>{(a) => <BotAvatar avatar={a().avatar} size={18} alive={false} />}</Show>
                    </span>
                    <span class="palette-hit-text">
                      <span class="palette-hit-head">
                        <span class="palette-hit-agent">{agent()?.name ?? "Familiar"}</span>
                        <span class="palette-hit-chat">{hit.chatTitle || "New chat"}</span>
                        <span class="palette-hit-time">{relativeTime(hit.createdAt)}</span>
                      </span>
                      <span class="palette-snippet">
                        <Show when={hit.role === "user"}>
                          <span class="palette-you">You: </span>
                        </Show>
                        <Parts parts={snippetParts(hit.snippet)} />
                      </span>
                    </span>
                  </Row>
                );
              }}
            </For>
          </Show>
          <Show when={!options().length}>
            <div class="palette-empty">{searching() ? "Searching messages…" : `Nothing matches "${query().trim()}".`}</div>
          </Show>
        </div>
        <div class="palette-footer" aria-hidden="true">
          <span>
            <kbd>↑</kbd>
            <kbd>↓</kbd> move
          </span>
          <span>
            <kbd>↵</kbd> open
          </span>
          <Show when={searching()}>
            <span class="palette-busy">Searching messages…</span>
          </Show>
        </div>
      </div>
    </div>
  );
}

function SpellGlyph() {
  return (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" aria-hidden="true">
      <circle cx="3.5" cy="8" r="1.8" />
      <circle cx="12.5" cy="8" r="1.8" />
      <path d="M5.5 8h5" />
    </svg>
  );
}
