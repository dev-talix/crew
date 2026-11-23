import { listen } from "@tauri-apps/api/event";
import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { createStore } from "solid-js/store";
import BotAvatar, { type Mood } from "./BotAvatar";
import { api, describeSchedule, onEvent, PROVIDER_NAMES, relativeTime, type Agent, type Message, type Routine } from "./api";
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
  busy: Record<string, boolean>;
  mood: (agentId: string) => Mood;
  onOpenAgent: (id: string) => void;
  onNewAgent: () => void;
  onOpenRoutines: () => void;
}) {
  const [last, setLast] = createStore<Record<string, Message>>({});
  const [routines, setRoutines] = createSignal<Routine[]>([]);
  const [now, setNow] = createSignal(Date.now());

  const loadRoutines = () => api.listRoutines().then(setRoutines).catch(() => undefined);

  onMount(async () => {
    const messages = await api.lastMessages().catch(() => [] as Message[]);
    messages.forEach((m) => setLast(m.agentId, m));
    void loadRoutines();
    const stops = await Promise.all([
      listen<Message>("message", ({ payload }) => {
        if ((payload.role === "user" || payload.role === "assistant") && payload.status !== "streaming") {
          setLast(payload.agentId, payload);
        }
      }),
      onEvent("routines_changed", loadRoutines),
    ]);
    onCleanup(() => stops.forEach((stop) => stop()));
  });

  const tick = setInterval(() => setNow(Date.now()), 60_000);
  onCleanup(() => clearInterval(tick));

  const agentById = (id: string) => props.agents.find((a) => a.id === id);
  const working = () => props.agents.filter((a) => props.busy[a.id]).length;
  const upcoming = createMemo(() =>
    routines()
      .filter((r) => r.enabled && r.nextRunAt && agentById(r.agentId))
      .sort((a, b) => (a.nextRunAt ?? 0) - (b.nextRunAt ?? 0))
      .slice(0, 5),
  );
  const summary = () => {
    const count = props.agents.length;
    const busy = working();
    const agents = `${count} ${count === 1 ? "agent" : "agents"}`;
    return busy ? `${agents}, ${busy} working now` : `${agents}, all idle`;
  };

  return (
    <Page
      title="Home"
      actions={
        <button class="ghost small" onClick={props.onNewAgent}>
          <PlusIcon /> New agent
        </button>
      }
    >
      <Show
        when={props.agents.length}
        fallback={
          <EmptyState
            title="Build your crew"
            body="Each agent gets its own workspace folder and runs on Codex or Claude."
            action={
              <button class="primary" onClick={props.onNewAgent}>
                Create your first agent
              </button>
            }
          />
        }
      >
        <div class="home">
          <h2 class="section-title home-summary">{summary()}</h2>
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
                    <Show when={last[agent.id]}>{(m) => <span>{relativeTime(m().createdAt, now())}</span>}</Show>
                  </span>
                </button>
              )}
            </For>
          </div>

          <h2 class="section-title">Coming up</h2>
          <div class="list">
            <Show
              when={upcoming().length}
              fallback={
                <div class="list-row">
                  <span class="list-main list-sub">No routines scheduled.</span>
                  <button class="ghost small" onClick={props.onOpenRoutines}>
                    Set up a routine
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
        </div>
      </Show>
    </Page>
  );
}
