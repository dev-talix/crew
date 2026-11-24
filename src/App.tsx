import { createEffect, createMemo, createSignal, createStore, For, Match, onSettled, Show, Switch } from "solid-js";
import type { JSX } from "@solidjs/web";
import AgentForm from "./AgentForm";
import Approvals from "./Approvals";
import BotAvatar from "./BotAvatar";
import Chat, { type ChatIntent } from "./Chat";
import CommandPalette, { type PaletteActions } from "./CommandPalette";
import Drafts from "./Drafts";
import { createChatStore } from "./chatStore";
import HomePage from "./HomePage";
import HandoffsPage from "./Handoffs";
import InboxPage, { InboxLink } from "./Inbox";
import { MemorySheet } from "./MemoryEditor";
import RoutinesPage from "./RoutinesPage";
import { SettingsDialog } from "./SettingsPage";
import WorkflowsPage, { type SpellIntent } from "./WorkflowsPage";
import { api, errorText, onEvent, onMountAsync, PROVIDER_NAMES, subscribe, withAgent, type Agent, type Coven, type Workflow } from "./api";
import { inkFor } from "./avatar";
import { groupByHome, homeOffline, isRemote, reloadsOpenChat } from "./linked";
import { runningModelName } from "./models";
import { createMoods } from "./moods";
import { ClockIcon, GearIcon, HomeIcon, PlusIcon, TalkIcon } from "./icons";
import { Sheet } from "./ui";
import { shownIcon } from "./themeStore";
import { startUpdateChecks, updateCheck } from "./updateStore";
import type { SettingsSection } from "./SettingsPage";

type View = "home" | "inbox" | "handoffs" | "routines" | "workflows" | "new" | { agent: string };

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
  // Bumped on every summon, so the form starts empty even when it's already open.
  const [summonCount, setSummonCount] = createSignal(1);
  const summon = () => {
    setSummonCount((n) => n + 1);
    setView("new");
  };
  const [editing, setEditing] = createSignal(false);
  const [memoryOpen, setMemoryOpen] = createSignal(false);
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
  // Linked Macs' familiars can't be used in rituals, spells, covens, the vault, or the dock.
  const localAgents = createMemo(() => agents().filter((a) => !isRemote(a)));
  // The sidebar lists this Mac's familiars coven by coven once there are covens, with the unbound last,
  // then each linked Mac's familiars under that Mac's name.
  const sidebarGroups = createMemo(() => {
    const remote = groupByHome(agents())
      .filter((m) => m.peerId !== null)
      .map((m) => ({ label: m.online ? m.name : `${m.name} · Offline`, agents: m.agents }));
    const list = localAgents();
    if (!covens().length) return [{ label: null as string | null, agents: list }, ...remote].filter((g) => g.agents.length > 0);
    const groups = covens().map((c) => ({ label: c.name as string | null, agents: list.filter((a) => a.covenId === c.id) }));
    const unbound = list.filter((a) => !a.covenId || !covens().some((c) => c.id === a.covenId));
    return [...groups, { label: "Unbound", agents: unbound }, ...remote].filter((g) => g.agents.length > 0);
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
  createEffect(selected, (id) => {
    api.setOpenAgent(id).catch(() => {});
  });
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
    setMemoryOpen(false);
    setView({ agent: id });
  };

  onMountAsync(async (keep) => {
    try {
      const [list, running] = await Promise.all([api.listAgents(), api.busyAgents()]);
      setAgents(list);
      setBusy((b) => {
        for (const id of running) b[id] = true;
      });
      if (!list.length) summon();
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
    // A linked Mac coming or going changes which familiars show and whether they're offline.
    for (const name of ["agents_changed", "links_changed"] as const) {
      keep(await onEvent<null>(name, () => api.listAgents().then(setAgents, () => {})));
    }
    // This Mac missed a linked Mac's events, or that Mac switched a familiar to another chat:
    // load the open chat again rather than adding to what it shows.
    keep(
      await onEvent<{ agentId: string | null }>("link_reload", ({ agentId }) => {
        if (reloadsOpenChat(agentId, current())) void chat.reload();
        api.listAgents().then(setAgents, () => {});
      }),
    );
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
      summon();
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

  // A familiar saved while its Mac was offline opens later, so the list may already have it, newer.
  const created = (agent: Agent) => {
    setAgents((list) => withAgent(list, agent));
    openAgent(agent.id);
    if (agent.setupPending) void api.listAgents().then(setAgents, () => {});
  };

  const imported = (id: string) =>
    void api.listAgents().then((list) => {
      setAgents(list);
      openAgent(id);
    }, () => {});

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
    summon: () => away(summon),
    newChat: (agentId) => away(() => openChatWith({ agentId, fresh: true })),
    openMessage: (hit) => away(() => openChatWith({ agentId: hit.agentId, chatId: hit.chatId, messageId: hit.messageId })),
  };

  const status = (agent: Agent) => {
    if (homeOffline(agent)) return "Offline";
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
          <InboxLink active={is("inbox")} onClick={() => setView("inbox")} />
          <NavItem label="Handoffs" icon={<TalkIcon />} active={is("handoffs")} onClick={() => setView("handoffs")} />
          <NavItem label="Rituals" icon={<ClockIcon />} active={is("routines")} onClick={() => setView("routines")} />
        </nav>
        <div class="sidebar-caption">
          <span>Familiars</span>
          <button class="icon-button" aria-label="Summon a familiar" title="Summon a familiar (⌘N)" onClick={summon}>
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
              const label = agent.home ? `${agent.name} on ${agent.home.name}` : agent.name;
              return (
              <button
                class={["agent-row", { active: agent.id === selected() }]}
                data-slot="sidebar-row"
                aria-current={agent.id === selected() ? "page" : undefined}
                title={index() < 9 ? `${label} (⌘${index() + 1})` : label}
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
            <button class="icon-button" aria-label="New spell" title="New spell" disabled={!localAgents().length} onClick={() => openSpells({ create: true })}>
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
                    onMemory={() => setMemoryOpen(true)}
                    onAgentUpdated={replaceAgent}
                    intent={chatIntent()}
                    onIntentDone={() => setChatIntent(null)}
                  />
                  <Show when={editing()}>
                    <Sheet label="Familiar settings" wide onClose={() => setEditing(false)}>
                      <AgentForm agent={agent()} covens={covens()} onSaved={updated} onCancel={() => setEditing(false)} onDeleted={deleted} />
                    </Sheet>
                  </Show>
                  <Show when={memoryOpen()}>
                    <MemorySheet agent={agent()} agents={agents()} covens={covens()} onClose={() => setMemoryOpen(false)} />
                  </Show>
                </div>
              )}
            </Match>
            <Match when={is("new")}>
              <div class="form-page" data-tauri-drag-region>
                <Show when={summonCount()} keyed>
                  {(_count) => <AgentForm covens={covens()} onSaved={created} onImported={imported} onCancel={agents().length ? () => setView("home") : undefined} />}
                </Show>
              </div>
            </Match>
            <Match when={is("inbox")}>
              <InboxPage agents={agents()} onOpenAgent={openAgent} />
            </Match>
            <Match when={is("handoffs")}>
              <HandoffsPage agents={agents()} onOpenAgent={openAgent} />
            </Match>
            <Match when={is("routines")}>
              <RoutinesPage agents={localAgents()} onOpenAgent={openAgent} />
            </Match>
            <Match when={is("workflows")}>
              <WorkflowsPage agents={localAgents()} onOpenAgent={openAgent} intent={spellIntent()} onIntentDone={() => setSpellIntent(null)} />
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
                onNewAgent={summon}
                onOpenRoutines={() => setView("routines")}
              />
            </Match>
          </Switch>
        </Show>
      </main>
      <Show when={settings()}>
        {(section) => <SettingsDialog agents={localAgents()} section={section()} onClose={() => setSettings(null)} />}
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
      <Drafts agents={agents()} />
    </div>
  );
}
