import { listen } from "@tauri-apps/api/event";
import { createMemo, createSignal, createStore, For, onCleanup, Show } from "solid-js";
import BotAvatar, { type Mood } from "./BotAvatar";
import { api, describeSchedule, onEvent, onMountAsync, PROVIDER_NAMES, relativeTime, type Agent, type Message, type Routine, type Workflow } from "./api";
import HomeCircle from "./HomeCircle";
import { PlusIcon } from "./icons";
import { EmptyState, Page } from "./ui";

const lowerFirst = (text: string) => text.charAt(0).toLowerCase() + text.slice(1);

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
  const upcoming = createMemo(() =>
    routines()
      .filter((r) => r.enabled && r.nextRunAt && agentById(r.agentId))
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
          <HomeCircle
            agents={props.agents}
            spells={props.spells}
            routines={routines()}
            busy={props.busy}
            mood={props.mood}
            now={now()}
            onOpenAgent={props.onOpenAgent}
            onOpenSpells={props.onOpenSpells}
          />
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
            <For each={props.agents}>
              {(agent) => (
                <button class="list-row home-agent" style={{ "--agent": agent.avatar.color }} onClick={() => props.onOpenAgent(agent.id)}>
                  <BotAvatar avatar={agent.avatar} size={32} mood={props.mood(agent.id)} />
                  <span class="list-main">
                    <span class="list-title">{agent.name}</span>
                    <span class="list-sub">{props.busy[agent.id] ? "Working" : preview(last[agent.id])}</span>
                  </span>
                  <span class="home-meta">
                    <span>{PROVIDER_NAMES[agent.provider]}</span>
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
                  const agent = () => agentById(routine.agentId)!;
                  return (
                    <button class="list-row home-agent" onClick={props.onOpenRoutines}>
                      <BotAvatar avatar={agent().avatar} size={24} />
                      <span class="list-main">
                        <span class="list-title">{routine.name}</span>
                        <span class="list-sub">
                          {agent().name}, {lowerFirst(describeSchedule(routine.schedule))}
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
    </Page>
  );
}
