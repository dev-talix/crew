import { listen } from "@tauri-apps/api/event";
import { createMemo, createSignal, createStore, For, onCleanup, Show } from "solid-js";
import BotAvatar, { type Mood } from "./BotAvatar";
import { api, describeSchedule, onEvent, onMountAsync, PROVIDER_NAMES, relativeTime, type Agent, type Message, type Coven, type Routine, type Workflow } from "./api";
import CovenEditor from "./CovenEditor";
import { RitualPauseNotice } from "./BudgetControls";
import HomeCircle from "./HomeCircle";
import { PlusIcon } from "./icons";
import { EmptyState, Page, Sheet } from "./ui";
import { ritualAgents, ritualWho } from "./spells";

const lowerFirst = (text: string) => text.charAt(0).toLowerCase() + text.slice(1);

const COVEN_KEY = "grimoire-home-coven";

function readCoven(): string | null {
  try {
    return localStorage.getItem(COVEN_KEY);
  } catch {
    return null;
  }
}

function writeCoven(id: string | null) {
  try {
    if (id) localStorage.setItem(COVEN_KEY, id);
    else localStorage.removeItem(COVEN_KEY);
  } catch {
    // The choice still applies until Grimoire quits.
  }
}

function preview(message: Message | undefined): string {
  if (!message) return "No messages yet";
  const line = message.text
    .split("\n")
    .map((l) => l.replace(/^[#>*\-\s]+/, "").replace(/[`*_]/g, "").trim())
    .find((l) => l.length > 0);
  return message.role === "user" ? `You: ${line ?? ""}` : (line ?? "");
}

export default function HomePage(props: {
  agents: Agent[];
  spells: Workflow[];
  covens: Coven[];
  busy: Record<string, boolean>;
  mood: (agentId: string) => Mood;
  onOpenAgent: (id: string) => void;
  onNewAgent: () => void;
  onOpenRoutines: () => void;
  onOpenSpells: () => void;
}) {
  const [last, setLast] = createStore<Record<string, Message>>({});
  const [routines, setRoutines] = createSignal<Routine[]>([]);
  const [now, setNow] = createSignal(Date.now());
  // A reply can land after the clock last ticked; it still happened, so never call it "in a moment".
  const ago = (ms: number) => (ms >= now() - 30_000 ? "just now" : relativeTime(ms, now()));

  const loadRoutines = () => api.listRoutines().then(setRoutines).catch(() => undefined);

  onMountAsync(async (keep) => {
    const messages = await api.lastMessages().catch(() => [] as Message[]);
    setLast((last) => {
      for (const m of messages) last[m.agentId] = m;
    });
    void loadRoutines();
    const stops = await Promise.all([
      listen<Message>("message", ({ payload }) => {
        if ((payload.role === "user" || payload.role === "assistant") && payload.status !== "streaming") {
          setLast((last) => {
            last[payload.agentId] = payload;
          });
        }
      }),
      onEvent("routines_changed", loadRoutines),
    ]);
    stops.forEach(keep);
  });

  const tick = setInterval(() => setNow(Date.now()), 60_000);
  onCleanup(() => clearInterval(tick));

  const agentById = (id: string) => props.agents.find((a) => a.id === id);

  // Which coven the circle shows: null for everyone. Remembered between launches.
  const [selected, setSelected] = createSignal<string | null>(readCoven());
  const [editing, setEditing] = createSignal<Coven | "new" | null>(null);
  const choose = (id: string | null) => {
    setSelected(id);
    writeCoven(id);
  };
  const coven = () => props.covens.find((c) => c.id === selected()) ?? null;
  const members = (id: string) => props.agents.filter((a) => a.covenId === id);
  // Everyone, seated coven by coven so each coven's arc is one piece, with the unbound last.
  const everyone = createMemo(() => [
    ...props.covens.flatMap((c) => members(c.id)),
    ...props.agents.filter((a) => !a.covenId || !props.covens.some((c) => c.id === a.covenId)),
  ]);
  const shown = () => (coven() ? members(coven()!.id) : everyone());
  const shownIds = () => new Set(shown().map((a) => a.id));
  const upcoming = createMemo(() =>
    routines()
      .filter((r) => r.enabled && r.nextRunAt && ritualAgents(r, props.spells).some((id) => shownIds().has(id)))
      .sort((a, b) => (a.nextRunAt ?? 0) - (b.nextRunAt ?? 0))
      .slice(0, 5),
  );

  return (
    <Page
      title="Home"
      actions={
        <button class="ghost small" onClick={props.onNewAgent}>
          <PlusIcon /> Summon a familiar
        </button>
      }
    >
      <Show
        when={props.agents.length}
        fallback={
          <EmptyState
            title="Summon your first familiar"
            body="Each familiar gets its own workspace folder and runs on Codex or Claude."
            action={
              <button class="primary" onClick={props.onNewAgent}>
                Summon a familiar
              </button>
            }
          />
        }
      >
        <div class="home">
          <RitualPauseNotice />
          <div class="coven-chips" role="tablist" aria-label="Covens">
            <button role="tab" aria-selected={!coven() ? "true" : "false"} class={["coven-chip", { on: !coven() }]} onClick={() => choose(null)}>
              All <span class="coven-chip-count">{props.agents.length}</span>
            </button>
            <For each={props.covens}>
              {(c) => (
                <button role="tab" aria-selected={coven()?.id === c.id ? "true" : "false"} class={["coven-chip", { on: coven()?.id === c.id }]} onClick={() => choose(c.id)}>
                  {c.name} <span class="coven-chip-count">{members(c.id).length}</span>
                </button>
              )}
            </For>
            <button class="coven-chip add" aria-label="New coven" title="New coven" onClick={() => setEditing("new")}>
              <PlusIcon />
            </button>
          </div>

          <HomeCircle
            agents={shown()}
            spells={props.spells}
            routines={routines()}
            busy={props.busy}
            mood={props.mood}
            now={now()}
            covens={coven() ? undefined : props.covens}
            title={coven()?.name}
            centerAction={
              <Show when={coven() && shown().length === 0}>
                <button class="ghost small" onClick={() => setEditing(coven())}>
                  Add familiars
                </button>
              </Show>
            }
            onOpenAgent={props.onOpenAgent}
            onOpenSpells={props.onOpenSpells}
          />
          <Show when={coven()}>
            {(c) => (
              <div class="coven-edit">
                <button class="ghost small" onClick={() => setEditing(c())}>
                  Edit {c().name}
                </button>
              </div>
            )}
          </Show>
          <p class="home-circle-legend">
            <span>
              <i class="legend-line" aria-hidden="true" /> Spells link familiars
            </span>
            <span>
              <i class="legend-arc" aria-hidden="true" /> Gold marks a ritual
            </span>
          </p>

          <div class="home-columns">
            <section>
              <h2 class="section-title">Latest replies</h2>
              <div class="list">
                <For each={shown()} fallback={<p class="list-row list-sub">No familiars in this coven yet.</p>}>
                  {(agent) => (
                    <button class="list-row home-agent" style={{ "--agent": agent.avatar.color }} onClick={() => props.onOpenAgent(agent.id)}>
                      <BotAvatar avatar={agent.avatar} size={32} mood={props.mood(agent.id)} />
                      <span class="list-main">
                        <span class="list-title">{agent.name}</span>
                        <span class="list-sub">{props.busy[agent.id] ? "Working" : preview(last[agent.id])}</span>
                      </span>
                      <span class="home-meta">
                        <span>{PROVIDER_NAMES[agent.provider]}</span>
                        <Show when={agent.host}>{(host) => <span class="remote-badge">{host()}</span>}</Show>
                        <Show when={last[agent.id]}>{(m) => <span>{ago(m().createdAt)}</span>}</Show>
                      </span>
                    </button>
                  )}
                </For>
              </div>
            </section>
            <section>
              <h2 class="section-title">Coming up</h2>
              <div class="list">
                <Show
                  when={upcoming().length}
                  fallback={
                    <div class="list-row">
                      <span class="list-main list-sub">No rituals scheduled.</span>
                      <button class="ghost small" onClick={props.onOpenRoutines}>
                        Set up a ritual
                      </button>
                    </div>
                  }
                >
                  <For each={upcoming()}>
                    {(routine) => {
                      // A spell ritual shows the familiar its spell starts with.
                      const agent = () => agentById(ritualAgents(routine, props.spells)[0] ?? "");
                      return (
                        <button class="list-row home-agent" onClick={props.onOpenRoutines}>
                          <Show when={agent()}>{(a) => <BotAvatar avatar={a().avatar} size={24} />}</Show>
                          <span class="list-main">
                            <span class="list-title">{routine.name}</span>
                            <span class="list-sub">
                              {ritualWho(routine, props.agents, props.spells)}, {lowerFirst(describeSchedule(routine.schedule))}
                            </span>
                          </span>
                          <span class="home-meta">
                            <span>{relativeTime(routine.nextRunAt!, now())}</span>
                          </span>
                        </button>
                      );
                    }}
                  </For>
                </Show>
              </div>
            </section>
          </div>
        </div>
      </Show>
      <Show when={editing()}>
        {(target) => (
          <Sheet label={target() === "new" ? "New coven" : "Edit coven"} wide onClose={() => setEditing(null)}>
            <CovenEditor
              coven={target() === "new" ? null : (target() as Coven)}
              agents={props.agents}
              covens={props.covens}
              onSaved={(saved) => {
                setEditing(null);
                choose(saved.id);
              }}
              onDeleted={() => {
                setEditing(null);
                choose(null);
              }}
              onCancel={() => setEditing(null)}
            />
          </Sheet>
        )}
      </Show>
    </Page>
  );
}
