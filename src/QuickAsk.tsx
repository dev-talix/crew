import { getCurrentWindow } from "@tauri-apps/api/window";
import { createMemo, createSignal, createStore, flush, For, onSettled, Show } from "solid-js";
import BotAvatar from "./BotAvatar";
import Chat from "./Chat";
import { createChatStore } from "./chatStore";
import { api, onEvent, onMountAsync, subscribe, type Agent } from "./api";
import { inkFor } from "./avatar";
import { createInbox, InboxList } from "./Inbox";
import { isRemote } from "./linked";
import { pickAgent } from "./quick";
import { EmptyState } from "./ui";
import "./quick.css";

/** The panel the global shortcut opens: ask a familiar something, or answer what's waiting. */
export default function QuickAsk() {
  const [all, setAgents] = createSignal<Agent[]>([]);
  // Like the dock, only this Mac's familiars.
  const agents = createMemo(() => all().filter((a) => !isRemote(a)));
  const [selected, setSelected] = createSignal<string | null>(null);
  const [busy, setBusy] = createStore<Record<string, boolean>>({});
  const [view, setView] = createSignal<"ask" | "inbox">("ask");
  const chat = createChatStore(selected);
  const waiting = createInbox().items;
  const current = () => agents().find((a) => a.id === selected());

  const focusComposer = () => {
    if (view() === "ask") document.querySelector<HTMLTextAreaElement>(".quick .chat textarea")?.focus();
  };
  const choose = (id: string) => {
    setSelected(id);
    setView("ask");
    void api.setQuickAgent(id).catch(() => undefined);
    requestAnimationFrame(focusComposer);
  };
  const hide = () => void api.quickHide().catch(() => undefined);
  const openInGrimoire = async (id: string) => {
    await api.showMain(id).catch(() => undefined);
    hide();
  };

  const refreshAgents = async () => {
    setAgents(await api.listAgents().catch(() => [] as Agent[]));
    flush(); // agents() below reads the new list.
    setSelected((id) => pickAgent(agents().map((a) => a.id), id));
  };

  onMountAsync(async (keep) => {
    const [list, running, saved] = await Promise.all([api.listAgents(), api.busyAgents(), api.quickAgent().catch(() => null)]);
    setAgents(list);
    setBusy((b) => {
      for (const id of running) b[id] = true;
    });
    flush();
    setSelected(pickAgent(agents().map((a) => a.id), saved));
    requestAnimationFrame(focusComposer);
    const stops = await Promise.all([
      subscribe({
        message: chat.onMessage,
        delta: chat.onDelta,
        status: ({ agentId, busy: running }) =>
          setBusy((b) => {
            b[agentId] = running;
          }),
      }),
      onEvent<null>("agents_changed", refreshAgents),
      onEvent<{ agentId: string; model: string }>("agent_model", ({ agentId, model }) =>
        setAgents((list) => list.map((a) => (a.id === agentId ? { ...a, activeModel: model } : a))),
      ),
      // The shortcut can fire from any app, so typing should land in the composer right away.
      getCurrentWindow().onFocusChanged(({ payload: focused }) => focused && focusComposer()),
    ]);
    stops.forEach(keep);
  });

  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape" && !e.defaultPrevented) {
      e.preventDefault();
      hide();
    }
  };
  onSettled(() => {
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const color = () => current()?.avatar.color;

  return (
    <div class="quick" style={color() ? { "--agent": color()!, "--agent-ink": inkFor(color()!) } : undefined}>
      <header class="quick-bar" data-tauri-drag-region>
        <div class="quick-familiars" role="group" aria-label="Familiars">
          <For each={agents()}>
            {(agent) => {
              const active = () => view() === "ask" && selected() === agent.id;
              return (
                <button
                  class={["quick-familiar", { active: active() }]}
                  style={{ "--agent": agent.avatar.color }}
                  aria-label={agent.name}
                  aria-pressed={active() ? "true" : "false"}
                  title={agent.name}
                  onClick={() => choose(agent.id)}
                >
                  <BotAvatar avatar={agent.avatar} size={26} working={!!busy[agent.id]} alive={false} />
                </button>
              );
            }}
          </For>
        </div>
        <button
          class={["ghost small quick-inbox-tab", { active: view() === "inbox" }]}
          aria-pressed={view() === "inbox" ? "true" : "false"}
          aria-label={waiting().length ? `Inbox, ${waiting().length} waiting` : "Inbox"}
          onClick={() => setView((v) => (v === "inbox" ? "ask" : "inbox"))}
        >
          Inbox
          <Show when={waiting().length}>
            <span class="inbox-badge">{waiting().length}</span>
          </Show>
        </button>
      </header>

      <Show when={view() === "inbox"}>
        <div class="quick-inbox">
          <InboxList agents={all()} onOpenAgent={choose} />
        </div>
      </Show>
      {/* Hidden rather than unmounted behind the inbox, so the chat keeps following chat switches. */}
      <div class="quick-ask" hidden={view() !== "ask"}>
        <Show
          when={current()}
          fallback={<EmptyState title="No familiars yet" body="Make a familiar in Grimoire, then ask it things from here." />}
        >
          {(agent) => (
            <Chat
              agent={agent()}
              busy={!!busy[agent().id]}
              store={chat}
              onEdit={() => void openInGrimoire(agent().id)}
              onAgentUpdated={(updated) => setAgents((list) => list.map((a) => (a.id === updated.id ? updated : a)))}
              compact={{ onClose: hide, onOpenMain: () => void openInGrimoire(agent().id) }}
            />
          )}
        </Show>
      </div>
    </div>
  );
}
