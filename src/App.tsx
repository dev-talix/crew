import { createEffect, createMemo, createSignal, createStore, For, Match, onSettled, Show, Switch } from "solid-js";
import type { JSX } from "@solidjs/web";
import AgentForm from "./AgentForm";
import Approvals from "./Approvals";
import BotAvatar from "./BotAvatar";
import Chat, { type ChatIntent } from "./Chat";
import CommandPalette, { type PaletteActions } from "./CommandPalette";
import { createChatStore } from "./chatStore";
import HomePage from "./HomePage";
import RoutinesPage from "./RoutinesPage";
import { SettingsDialog } from "./SettingsPage";
import WorkflowsPage, { type SpellIntent } from "./WorkflowsPage";
import { api, errorText, onEvent, onMountAsync, PROVIDER_NAMES, subscribe, type Agent, type Coven, type Workflow } from "./api";
import { inkFor } from "./avatar";
import { runningModelName } from "./models";
import { createMoods } from "./moods";
import { ClockIcon, GearIcon, HomeIcon, PlusIcon } from "./icons";
import { Sheet } from "./ui";
import { shownIcon } from "./themeStore";
import { startUpdateChecks, updateCheck } from "./updateStore";
import type { SettingsSection } from "./SettingsPage";

type View = "home" | "routines" | "workflows" | "new" | { agent: string };

function NavItem(props: { label: string; icon: JSX.Element; active: boolean; onClick: () => void; shortcut?: string }) {
  return (
    <button
      class={["nav-item", { active: props.active }]}
      aria-current={props.active ? "page" : undefined}
      title={props.shortcut ? `${props.label} (${props.shortcut})` : props.label}
      onClick={props.onClick}
    >
      {props.icon}
      <span>{props.label}</span>
    </button>
  );
}

export default function App() {
  const [agents, setAgents] = createSignal<Agent[]>([]);
  const [busy, setBusy] = createStore<Record<string, boolean>>({});
  const [unread, setUnread] = createStore<Record<string, boolean>>({});
  const [view, setView] = createSignal<View>("home");
  const [editing, setEditing] = createSignal(false);
  const [loaded, setLoaded] = createSignal(false);
  const [loadError, setLoadError] = createSignal("");
  const [spells, setSpells] = createSignal<Workflow[]>([]);
  const [spellIntent, setSpellIntent] = createSignal<SpellIntent | null>(null);
  // Settings is a dialog over whatever page is open; the value is the section it opens on.
  const [settings, setSettings] = createSignal<SettingsSection | null>(null);
  const openSettings = (section?: SettingsSection) => setSettings(section ?? "general");
  const agentById = (id: string) => agents().find((a) => a.id === id);
  const loadSpells = () => api.listWorkflows().then(setSpells, () => {});
  const [covens, setCovens] = createSignal<Coven[]>([]);
  const loadCovens = () => api.listCovens().then(setCovens, () => {});
  // The sidebar lists familiars coven by coven once there are covens, with the unbound last.
  const sidebarGroups = createMemo(() => {
    const list = agents();
    if (!covens().length) return [{ label: null as string | null, agents: list }];
    const groups = covens().map((c) => ({ label: c.name as string | null, agents: list.filter((a) => a.covenId === c.id) }));
    const unbound = list.filter((a) => !a.covenId || !covens().some((c) => c.id === a.covenId));
    return [...groups, { label: "Unbound", agents: unbound }].filter((g) => g.agents.length > 0);
  });
  const ordered = () => sidebarGroups().flatMap((g) => g.agents);
  const [palette, setPalette] = createSignal(false);
  const [chatIntent, setChatIntent] = createSignal<ChatIntent | null>(null);
  const openSpells = (intent: SpellIntent | null) => {
    setSpellIntent(intent);
    setView("workflows");
  };

  const selected = () => {
    const v = view();
    return typeof v === "object" ? v.agent : null;
  };
  const chat = createChatStore(selected);
  // The Dock icon follows the app icon setting, including the appearance when it is set to match.
  createEffect(shownIcon, (icon) => {
    api.setAppIcon(icon).catch(() => {});
  });
  const moods = createMoods();
  const moodOf = (id: string) => moods.mood(id, !!busy[id]);
  const current = () => agents().find((a) => a.id === selected());
  const is = (name: Exclude<View, { agent: string }>) => view() === name;

  const openAgent = (id: string) => {
    setUnread((u) => {
      u[id] = false;
    });
    setEditing(false);
    setView({ agent: id });
  };

  onMountAsync(async (keep) => {
    try {
      const [list, running] = await Promise.all([api.listAgents(), api.busyAgents()]);
      setAgents(list);
      setBusy((b) => {
        for (const id of running) b[id] = true;
      });
      if (!list.length) setView("new");
    } catch (e) {
      setLoadError(errorText(e));
    } finally {
      setLoaded(true);
    }
    const stopModels = await onEvent<{ agentId: string; model: string }>("agent_model", ({ agentId, model }) =>
      setAgents((list) => list.map((a) => (a.id === agentId ? { ...a, activeModel: model } : a))),
    );
    keep(stopModels);
    keep(await onEvent("workflows_changed", () => void loadSpells()));
    keep(await onEvent("covens_changed", () => void loadCovens()));
    void loadCovens();
    startUpdateChecks();
    void loadSpells();
    const stopOpen = await onEvent<string>("open_agent", (id) => {
      if (agents().some((a) => a.id === id)) openAgent(id);
    });
    keep(stopOpen);
    const stopAgents = await onEvent<null>("agents_changed", () => api.listAgents().then(setAgents));
    keep(stopAgents);
    const stop = await subscribe({
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
    });
    keep(stop);
  });

  const onKey = (e: KeyboardEvent) => {
    if (!e.metaKey || e.altKey || e.ctrlKey) return;
    if (e.key === "k") {
      e.preventDefault();
      setPalette((open) => !open);
    } else if (e.key === "n") {
      e.preventDefault();
      setView("new");
    } else if (e.key === ",") {
      e.preventDefault();
      setSettings((open) => (open ? null : "general"));
    } else if (/^[1-9]$/.test(e.key)) {
      const agent = ordered()[Number(e.key) - 1];
      if (agent) {
        e.preventDefault();
        openAgent(agent.id);
      }
    }
  };
  onSettled(() => {
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const created = (agent: Agent) => {
    setAgents((list) => [...list, agent]);
    openAgent(agent.id);
  };

  const replaceAgent = (agent: Agent) => setAgents((list) => list.map((a) => (a.id === agent.id ? agent : a)));

  const updated = (agent: Agent) => {
    replaceAgent(agent);
    setEditing(false);
  };

  const deleted = (id: string) => {
    const rest = agents().filter((a) => a.id !== id);
    setAgents(rest);
    setEditing(false);
    setView(rest.length ? "home" : "new");
  };

  // Every palette choice but a settings section leaves Settings.
  const away = (go: () => void) => {
    setSettings(null);
    go();
  };
  const openChatWith = (intent: ChatIntent) => {
    setChatIntent(intent);
    openAgent(intent.agentId);
  };
  const paletteActions: PaletteActions = {
    openAgent: (id) => away(() => openAgent(id)),
    castSpell: (id) => away(() => openSpells({ cast: id })),
    newSpell: () => away(() => openSpells({ create: true })),
    openRituals: () => away(() => setView("routines")),
    openHome: () => away(() => setView("home")),
    openSettings: (section) => setSettings(section),
    summon: () => away(() => setView("new")),
    newChat: (agentId) => away(() => openChatWith({ agentId, fresh: true })),
    openMessage: (hit) => away(() => openChatWith({ agentId: hit.agentId, chatId: hit.chatId, messageId: hit.messageId })),
  };

  const status = (agent: Agent) => {
    if (busy[agent.id]) return "Working";
    if (unread[agent.id]) return "New reply";
    const model = runningModelName(agent);
    return model === "Default model" ? PROVIDER_NAMES[agent.provider] : model;
  };

  return (
    <div class="app">
      <aside class="sidebar" data-slot="sidebar">
        <div class="sidebar-top" data-tauri-drag-region>
          <span class="brand" data-tauri-drag-region>
            Grimoire
          </span>
        </div>
        <nav class="nav" aria-label="Sections">
          <NavItem label="Home" icon={<HomeIcon />} active={is("home")} onClick={() => setView("home")} />
          <NavItem label="Rituals" icon={<ClockIcon />} active={is("routines")} onClick={() => setView("routines")} />
        </nav>
        <div class="sidebar-caption">
          <span>Familiars</span>
          <button class="icon-button" aria-label="Summon a familiar" title="Summon a familiar (⌘N)" onClick={() => setView("new")}>
            <PlusIcon />
          </button>
        </div>
        <nav class="agent-list" aria-label="Familiars and spells">
          <For each={sidebarGroups()}>
            {(group) => (
              <>
                <Show when={group.label}>
                  <div class="coven-subhead">{group.label}</div>
                </Show>
                <For each={group.agents}>
            {(agent) => {
              const index = () => ordered().indexOf(agent);
              return (
              <button
                class={["agent-row", { active: agent.id === selected() }]}
                data-slot="sidebar-row"
                aria-current={agent.id === selected() ? "page" : undefined}
                title={index() < 9 ? `${agent.name} (⌘${index() + 1})` : agent.name}
                style={{ "--agent": agent.avatar.color }}
                onClick={() => openAgent(agent.id)}
              >
                <BotAvatar avatar={agent.avatar} size={28} mood={moodOf(agent.id)} />
                <span class="agent-text">
                  <span class="agent-name">{agent.name}</span>
                  <span class={["agent-status", { live: busy[agent.id], unread: !busy[agent.id] && unread[agent.id] }]}>
                    {status(agent)}
                  </span>
                </span>
                <Show when={!busy[agent.id] && unread[agent.id]}>
                  <span class="unread-dot" aria-hidden="true" />
                </Show>
              </button>
              );
            }}
                </For>
              </>
            )}
          </For>
          <div class="sidebar-caption">
            <button class={["caption-link", { active: is("workflows") }]} aria-current={is("workflows") ? "page" : undefined} onClick={() => openSpells(null)}>
              Spells
            </button>
            <button class="icon-button" aria-label="New spell" title="New spell" disabled={!agents().length} onClick={() => openSpells({ create: true })}>
              <PlusIcon />
            </button>
          </div>
          <For each={spells()}>
            {(spell) => (
              <button class="spell-row" data-slot="sidebar-row" title={`Cast ${spell.name}`} onClick={() => openSpells({ cast: spell.id })}>
                <span class="spell-name">{spell.name}</span>
                <span class="spell-chain" aria-hidden="true">
                  <For each={spell.steps.slice(0, 4)}>
                    {(step) => <Show when={agentById(step.agentId)}>{(a) => <BotAvatar avatar={a().avatar} size={14} alive={false} />}</Show>}
                  </For>
                </span>
              </button>
            )}
          </For>
        </nav>
        <div class="sidebar-footer">
          <Show when={updateCheck()?.newer && updateCheck()?.latest}>
            {(latest) => (
              <button class="update-notice" onClick={() => openSettings("updates")}>
                Version {latest().version} is available
              </button>
            )}
          </Show>
          <NavItem label="Settings" icon={<GearIcon />} active={settings() !== null} onClick={() => openSettings()} shortcut="⌘," />
        </div>
      </aside>

      <main class="main">
        <Show when={loadError()}>
          <p class="error-box page-error" role="alert">
            {loadError()}
          </p>
        </Show>
        <Show when={loaded()}>
          <Switch>
            <Match when={current()}>
              {(agent) => (
                <div class="agent-scope" style={{ "--agent": agent().avatar.color, "--agent-ink": inkFor(agent().avatar.color) }}>
                  <Chat
                    agent={agent()}
                    busy={!!busy[agent().id]}
                    mood={moodOf(agent().id)}
                    store={chat}
                    onEdit={() => setEditing(true)}
                    onAgentUpdated={replaceAgent}
                    intent={chatIntent()}
                    onIntentDone={() => setChatIntent(null)}
                  />
                  <Show when={editing()}>
                    <Sheet label="Familiar settings" wide onClose={() => setEditing(false)}>
                      <AgentForm agent={agent()} covens={covens()} onSaved={updated} onCancel={() => setEditing(false)} onDeleted={deleted} />
                    </Sheet>
                  </Show>
                </div>
              )}
            </Match>
            <Match when={is("new")}>
              <div class="form-page" data-tauri-drag-region>
                <AgentForm covens={covens()} onSaved={created} onCancel={agents().length ? () => setView("home") : undefined} />
              </div>
            </Match>
            <Match when={is("routines")}>
              <RoutinesPage agents={agents()} onOpenAgent={openAgent} />
            </Match>
            <Match when={is("workflows")}>
              <WorkflowsPage agents={agents()} onOpenAgent={openAgent} intent={spellIntent()} onIntentDone={() => setSpellIntent(null)} />
            </Match>
            <Match when={true}>
              <HomePage
                agents={agents()}
                spells={spells()}
                covens={covens()}
                onOpenSpells={() => openSpells(null)}
                busy={busy}
                mood={moodOf}
                onOpenAgent={openAgent}
                onNewAgent={() => setView("new")}
                onOpenRoutines={() => setView("routines")}
              />
            </Match>
          </Switch>
        </Show>
      </main>
      <Show when={settings()}>
        {(section) => <SettingsDialog agents={agents()} section={section()} onClose={() => setSettings(null)} />}
      </Show>
      <Show when={palette()}>
        <CommandPalette
          agents={agents()}
          spells={spells()}
          current={current()}
          onClose={() => setPalette(false)}
          actions={paletteActions}
        />
      </Show>
      <Approvals agents={agents()} />
    </div>
  );
}
