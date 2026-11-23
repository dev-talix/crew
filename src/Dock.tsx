import { getCurrentWindow } from "@tauri-apps/api/window";
import { createMemo, createSignal, createStore, flush, For, onCleanup, onSettled, Show } from "solid-js";
import BotAvatar from "./BotAvatar";
import Chat from "./Chat";
import { createChatStore } from "./chatStore";
import { api, onEvent, onMountAsync, subscribe, type Agent } from "./api";
import { inkFor } from "./avatar";
import { GrimoireIcon } from "./icons";
import { createMoods } from "./moods";
import { runningModelName } from "./models";
import "./dock.css";

type Phase = "closed" | "opening" | "open" | "closing";

const PAD = 10;
const RAIL = 56;
const SLOT = 46;
const PANEL_WIDTH = 384;
const PANEL_HEIGHT = 600;
const GAP = 8;
const CLOSE_MS = 190;
const BLUR_GRACE_MS = 200;

const reducedMotion = () => window.matchMedia("(prefers-reduced-motion: reduce)").matches;
const nextFrame = () => new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));

export default function Dock() {
  const [agents, setAgents] = createSignal<Agent[]>([]);
  const [busy, setBusy] = createStore<Record<string, boolean>>({});
  const [unread, setUnread] = createStore<Record<string, boolean>>({});
  const [edge, setEdge] = createSignal<"left" | "right">("right");
  const [phase, setPhase] = createSignal<Phase>("closed");
  const [selected, setSelected] = createSignal<string | null>(null);
  const [entered, setEntered] = createSignal(false);
  const chat = createChatStore(selected);
  const moods = createMoods();
  let blurTimer: number | undefined;
  const current = () => agents().find((a) => a.id === selected());
  const panelVisible = () => phase() !== "closed";

  const railHeight = createMemo(() => PAD + agents().length * SLOT + (agents().length ? 13 : 0) + SLOT + PAD - 6);
  const closedSize = () => ({ width: RAIL + PAD * 2, height: railHeight() + PAD * 2 });
  const openSize = () => ({ width: RAIL + GAP + PANEL_WIDTH + PAD * 2, height: Math.max(PANEL_HEIGHT, railHeight()) + PAD * 2 });

  let busyResizing: Promise<void> = Promise.resolve();
  const resize = (size: { width: number; height: number }, focus: boolean) => {
    busyResizing = busyResizing.then(() => api.dockResize(size.width, size.height, focus).catch(() => undefined));
    return busyResizing;
  };

  const open = async (id: string) => {
    setUnread((u) => {
      u[id] = false;
    });
    if (phase() === "open" || phase() === "opening") {
      if (selected() === id) return close();
      setSelected(id);
      return;
    }
    setSelected(id);
    setPhase("opening");
    await resize(openSize(), true);
    await nextFrame();
    setPhase("open");
  };

  const close = async () => {
    if (phase() === "closed" || phase() === "closing") return;
    setPhase("closing");
    await new Promise((resolve) => setTimeout(resolve, reducedMotion() ? 0 : CLOSE_MS));
    if (phase() !== "closing") return;
    await resize(closedSize(), false);
    setPhase("closed");
    setSelected(null);
  };

  const openInGrimoire = async (id?: string) => {
    await api.showMain(id).catch(() => undefined);
    void close();
  };

  const refreshAgents = async () => {
    const list = await api.listAgents().catch(() => [] as Agent[]);
    setAgents(list);
    flush(); // closedSize() below reads the new agent count.
    if (selected() && !list.some((a) => a.id === selected())) void close();
    if (phase() === "closed") void resize(closedSize(), false);
  };

  onMountAsync(async (keep) => {
    const initial = await api.dockEdge().catch(() => null);
    if (initial) setEdge(initial);
    const [list, running] = await Promise.all([api.listAgents(), api.busyAgents()]);
    setAgents(list);
    setBusy((b) => {
      for (const id of running) b[id] = true;
    });
    flush(); // closedSize() reads the agent count.
    await resize(closedSize(), false);
    await nextFrame();
    setEntered(true);

    const stops = await Promise.all([
      subscribe({
        message: (message) => {
          chat.onMessage(message);
          moods.onMessage(message);
        },
        delta: chat.onDelta,
        status: ({ agentId, busy: running }) => {
          setBusy((b) => {
            b[agentId] = running;
          });
          moods.onStatus(agentId, running);
          if (!running && agentId !== selected()) {
            setUnread((u) => {
              u[agentId] = true;
            });
          }
        },
      }),
      onEvent<null>("agents_changed", refreshAgents),
      onEvent<{ agentId: string; model: string }>("agent_model", ({ agentId, model }) =>
        setAgents((list) => list.map((a) => (a.id === agentId ? { ...a, activeModel: model } : a))),
      ),
      onEvent<"left" | "right">("dock_edge", (next) => {
        setEdge(next);
        void resize(panelVisible() ? openSize() : closedSize(), false);
      }),
      getCurrentWindow().onFocusChanged(({ payload: focused }) => {
        clearTimeout(blurTimer);
        if (focused) return;
        blurTimer = window.setTimeout(async () => {
          const stillAway = !document.hasFocus() && !(await getCurrentWindow().isFocused().catch(() => false));
          if (stillAway) void close();
        }, BLUR_GRACE_MS);
      }),
    ]);
    stops.forEach(keep);
  });

  onCleanup(() => clearTimeout(blurTimer));

  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape" && panelVisible()) {
      e.preventDefault();
      void close();
    }
  };
  onSettled(() => {
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const status = (agent: Agent) => (busy[agent.id] ? "Working" : unread[agent.id] ? "New reply" : runningModelName(agent));

  return (
    <div data-edge={edge()} data-phase={phase()} class={["dock", { entered: entered() }]}>
      <nav class="dock-rail" data-slot="dock" aria-label="Familiars" style={{ height: `${railHeight()}px` }}>
        <For each={agents()}>
          {(agent, index) => (
            <button
              class={["dock-agent", { active: selected() === agent.id && panelVisible() }]}
              style={{ "--agent": agent.avatar.color, "--stagger": `${index() * 35}ms` }}
              aria-label={`${agent.name}, ${status(agent)}`}
              aria-pressed={selected() === agent.id && panelVisible() ? "true" : "false"}
              title={`${agent.name}: ${status(agent)}`}
              onClick={() => void open(agent.id)}
            >
              <BotAvatar avatar={agent.avatar} size={36} mood={moods.mood(agent.id, !!busy[agent.id])} fluid />
              <Show when={!busy[agent.id] && unread[agent.id]}>
                <span class="dock-dot" aria-hidden="true" />
              </Show>
            </button>
          )}
        </For>
        <Show when={agents().length}>
          <span class="dock-sep" aria-hidden="true" />
        </Show>
        <button class="dock-agent dock-home" aria-label="Open Grimoire" title="Open Grimoire" onClick={() => void openInGrimoire()}>
          <GrimoireIcon />
        </button>
      </nav>

      <Show when={panelVisible() && current()}>
        {(agent) => (
          <div
            class="dock-panel"
            style={{ "--agent": agent().avatar.color, "--agent-ink": inkFor(agent().avatar.color) }}
          >
            <Chat
              agent={agent()}
              busy={!!busy[agent().id]}
              mood={moods.mood(agent().id, !!busy[agent().id])}
              store={chat}
              onEdit={() => void openInGrimoire(agent().id)}
              onAgentUpdated={(updated) => setAgents((list) => list.map((a) => (a.id === updated.id ? updated : a)))}
              compact={{ onClose: () => void close(), onOpenMain: () => void openInGrimoire(agent().id) }}
            />
          </div>
        )}
      </Show>
    </div>
  );
}
