import { createMemo, createSignal, For, onCleanup, onSettled, Show } from "solid-js";
import BotAvatar from "./BotAvatar";
import { AgentSelect, EmptyState, Page, Segmented, Sheet, Toggle } from "./ui";
import { RitualArt, RITUAL_TEMPLATES, type RitualTemplate } from "./EmptyArt";
import { PlusIcon } from "./icons";
import { inkFor } from "./avatar";
import { api, describeSchedule, errorText, onEvent, relativeTime, type Agent, type Routine, type Schedule } from "./api";
import { RitualPauseNotice } from "./BudgetControls";
import type { RitualSkip } from "./budgets";
import "./routines.css";

const DAY_NAMES = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const ALL_DAYS = [1, 2, 3, 4, 5, 6, 7];
const WEEKDAYS = [1, 2, 3, 4, 5];
const WEEKENDS = [6, 7];
const MIN_MINUTES = 5;
const MAX_MINUTES = 7 * 24 * 60;

type Repeat = "interval" | "daily";
type Unit = "minutes" | "hours";

function agentStyle(agent: Agent) {
  return { "--agent": agent.avatar.color, "--agent-ink": inkFor(agent.avatar.color) };
}

function runsText(schedule: Schedule): string {
  const text = describeSchedule(schedule);
  return /^(Every|Weekdays|Weekends)/.test(text) ? `Runs ${text[0].toLowerCase()}${text.slice(1)}` : `Runs on ${text}`;
}

export default function RoutinesPage(props: { agents: Agent[]; onOpenAgent: (id: string) => void }) {
  const [routines, setRoutines] = createSignal<Routine[]>([]);
  const [loaded, setLoaded] = createSignal(false);
  const [error, setError] = createSignal("");
  const [editing, setEditing] = createSignal<Routine | "new" | { template: RitualTemplate } | null>(null);
  const [now, setNow] = createSignal(Date.now());
  const [skips, setSkips] = createSignal<RitualSkip[]>([]);

  const load = async () => {
    try {
      void api.ritualSkips().then(setSkips, () => undefined);
      setRoutines(await api.listRoutines());
      setError("");
    } catch (err) {
      setError(errorText(err));
    } finally {
      setLoaded(true);
    }
  };

  onSettled(() => void load());
  const unlisten = onEvent("routines_changed", () => void load());
  const tick = window.setInterval(() => setNow(Date.now()), 60_000);
  onCleanup(() => {
    clearInterval(tick);
    void unlisten.then((stop) => stop());
  });

  const groups = createMemo(() =>
    props.agents
      .map((agent) => ({ agent, routines: routines().filter((r) => r.agentId === agent.id) }))
      .filter((group) => group.routines.length > 0),
  );

  const upsert = (saved: Routine) =>
    setRoutines((list) => (list.some((r) => r.id === saved.id) ? list.map((r) => (r.id === saved.id ? saved : r)) : [...list, saved]));

  const setEnabled = async (routine: Routine, enabled: boolean) => {
    setError("");
    upsert({ ...routine, enabled });
    try {
      const { id, agentId, name, prompt, schedule } = routine;
      upsert(await api.saveRoutine({ id, agentId, name, prompt, schedule, enabled }));
    } catch (err) {
      upsert(routine);
      setError(errorText(err));
    }
  };

  const noAgents = () => props.agents.length === 0;
  const newButton = (primary: boolean) => (
    <button
      type="button"
      class={primary ? "primary" : "ghost"}
      disabled={noAgents()}
      title={noAgents() ? "Summon a familiar first" : undefined}
      onClick={() => setEditing("new")}
    >
      <PlusIcon />
      New ritual
    </button>
  );

  return (
    <Page title="Rituals" actions={newButton(true)}>
      <div class="routines">
        <RitualPauseNotice />
        <Show when={error()}>
          <p class="error-box" role="alert">
            {error()}
          </p>
        </Show>

        <Show when={loaded() && groups().length === 0}>
          <EmptyState
            art={<RitualArt agents={props.agents} />}
            title="No rituals yet"
            body={
              noAgents()
                ? "Summon a familiar first. A ritual sends it a message on a schedule."
                : "A ritual sends a familiar the same message on a schedule, even while you're away from the window."
            }
            action={newButton(false)}
          >
            <Show when={!noAgents()}>
              <div class="templates">
                <p class="templates-title">Or start from one of these</p>
                <For each={RITUAL_TEMPLATES}>
                  {(template) => (
                    <button type="button" class="template" onClick={() => setEditing({ template })}>
                      <span class="template-head">
                        <span class="list-title">{template.name}</span>
                        <span class="list-sub">{template.when}</span>
                      </span>
                      <span class="template-body">{template.prompt}</span>
                    </button>
                  )}
                </For>
              </div>
            </Show>
          </EmptyState>
        </Show>

        <For each={groups()}>
          {(group) => (
            <section class="routine-group" style={agentStyle(group.agent)}>
              <button type="button" class="routine-group-head" onClick={() => props.onOpenAgent(group.agent.id)}>
                <BotAvatar avatar={group.agent.avatar} size={28} />
                <span>{group.agent.name}</span>
              </button>
              <div class="list">
                <For each={group.routines}>
                  {(routine) => (
                    <RoutineRow
                      routine={routine}
                      skip={skips().find((s) => s.routineId === routine.id && s.at >= (routine.lastRunAt ?? 0))}
                      now={now()}
                      onEdit={() => setEditing(routine)}
                      onToggle={(enabled) => void setEnabled(routine, enabled)}
                      onError={setError}
                    />
                  )}
                </For>
              </div>
            </section>
          )}
        </For>
      </div>

      <Show when={editing()}>
        {(current) => (
          <RoutineEditor
            routine={typeof current() === "object" && "id" in (current() as object) ? (current() as Routine) : undefined}
            template={typeof current() === "object" && "template" in (current() as object) ? (current() as { template: RitualTemplate }).template : undefined}
            agents={props.agents}
            onClose={() => setEditing(null)}
            onSaved={(saved) => {
              upsert(saved);
              setEditing(null);
            }}
            onDeleted={(id) => {
              setRoutines((list) => list.filter((r) => r.id !== id));
              setEditing(null);
            }}
          />
        )}
      </Show>
    </Page>
  );
}

function RoutineRow(props: {
  routine: Routine;
  /** Set when the last run was skipped. */
  skip?: RitualSkip;
  now: number;
  onEdit: () => void;
  onToggle: (enabled: boolean) => void;
  onError: (message: string) => void;
}) {
  const [sent, setSent] = createSignal(false);
  const [running, setRunning] = createSignal(false);
  let sentTimer: number | undefined;
  onCleanup(() => clearTimeout(sentTimer));

  const runNow = async () => {
    setRunning(true);
    try {
      await api.runRoutine(props.routine.id);
      setSent(true);
      clearTimeout(sentTimer);
      sentTimer = window.setTimeout(() => setSent(false), 2000);
    } catch (err) {
      props.onError(errorText(err));
    } finally {
      setRunning(false);
    }
  };

  const nextRun = () => {
    if (!props.routine.enabled) return "Paused";
    const at = props.routine.nextRunAt;
    if (at === null) return "";
    return at <= props.now ? "Running soon" : `Next run ${relativeTime(at, props.now)}`;
  };

  return (
    <div class={["list-row routine-row", { paused: !props.routine.enabled }]}>
      <button type="button" class="list-main routine-open" aria-label={`Edit ${props.routine.name}`} onClick={props.onEdit}>
        <span class="list-title">{props.routine.name}</span>
        <span class="list-sub">{describeSchedule(props.routine.schedule)}</span>
        <Show when={props.skip}>
          {(skip) => (
            <span class="routine-skip">
              Skipped {relativeTime(skip().at, props.now)}. {skip().reason}
            </span>
          )}
        </Show>
      </button>
      <span class="routine-next">{nextRun()}</span>
      <div class="list-actions">
        <button type="button" class="ghost small routine-run" disabled={running()} onClick={() => void runNow()}>
          <span aria-live="polite">{sent() ? "Sent" : "Run now"}</span>
        </button>
        <Toggle
          label={`Run ${props.routine.name} on schedule`}
          checked={props.routine.enabled}
          onChange={props.onToggle}
        />
      </div>
    </div>
  );
}

function RoutineEditor(props: {
  routine?: Routine;
  /** Prefills a new ritual. */
  template?: RitualTemplate;
  agents: Agent[];
  onClose: () => void;
  onSaved: (routine: Routine) => void;
  onDeleted: (id: string) => void;
}) {
  const seed = props.routine ?? props.template;
  const initial = seed?.schedule;
  const initialMinutes = initial?.kind === "interval" ? initial.minutes : 180;
  const initialUnit: Unit = initialMinutes % 60 === 0 ? "hours" : "minutes";

  const [agentId, setAgentId] = createSignal(props.routine?.agentId ?? props.agents[0]?.id ?? "");
  const [name, setName] = createSignal(seed?.name ?? "");
  const [prompt, setPrompt] = createSignal(seed?.prompt ?? "");
  const [repeat, setRepeat] = createSignal<Repeat>(initial?.kind ?? "daily");
  const [amount, setAmount] = createSignal(String(initialUnit === "hours" ? initialMinutes / 60 : initialMinutes));
  const [unit, setUnit] = createSignal<Unit>(initialUnit);
  const [time, setTime] = createSignal(initial?.kind === "daily" ? initial.time : "09:00");
  // Shown days. An empty saved list means every day, so it shows all seven.
  const [days, setDays] = createSignal<number[]>(
    initial?.kind === "daily" ? (initial.days.length ? [...initial.days].sort() : ALL_DAYS) : WEEKDAYS,
  );
  const [error, setError] = createSignal("");
  const [saving, setSaving] = createSignal(false);
  const [confirmDelete, setConfirmDelete] = createSignal(false);
  let confirmTimer: number | undefined;
  onCleanup(() => clearTimeout(confirmTimer));

  const agent = () => props.agents.find((a) => a.id === agentId());

  const minutes = () => {
    const value = Number(amount());
    return Number.isInteger(value) ? value * (unit() === "hours" ? 60 : 1) : NaN;
  };
  const intervalValid = () => minutes() >= MIN_MINUTES && minutes() <= MAX_MINUTES;

  const schedule = (): Schedule | null => {
    if (repeat() === "interval") return intervalValid() ? { kind: "interval", minutes: minutes() } : null;
    if (!/^\d{2}:\d{2}$/.test(time())) return null;
    return { kind: "daily", time: time(), days: days().length === 7 ? [] : days() };
  };

  const toggleDay = (day: number) => {
    const current = days();
    if (current.includes(day)) {
      if (current.length > 1) setDays(current.filter((d) => d !== day));
    } else {
      setDays([...current, day].sort());
    }
  };

  const sameDays = (list: number[]) => days().join() === list.join();

  const canSave = () => !saving() && !!agent() && !!name().trim() && !!prompt().trim() && schedule() !== null;

  const submit = async (e: SubmitEvent) => {
    e.preventDefault();
    const s = schedule();
    if (!s) return;
    setError("");
    setSaving(true);
    try {
      const saved = await api.saveRoutine({
        id: props.routine?.id,
        agentId: agentId(),
        name: name().trim(),
        prompt: prompt().trim(),
        schedule: s,
        enabled: props.routine?.enabled ?? true,
      });
      props.onSaved(saved);
    } catch (err) {
      setError(errorText(err));
    } finally {
      setSaving(false);
    }
  };

  const remove = async () => {
    const routine = props.routine;
    if (!routine) return;
    if (!confirmDelete()) {
      setConfirmDelete(true);
      confirmTimer = window.setTimeout(() => setConfirmDelete(false), 4000);
      return;
    }
    try {
      await api.deleteRoutine(routine.id);
      props.onDeleted(routine.id);
    } catch (err) {
      setError(errorText(err));
    }
  };

  const title = props.routine ? "Edit ritual" : "New ritual";

  return (
    <Sheet label={title} onClose={props.onClose}>
      <form class="routine-form" style={agent() ? agentStyle(agent()!) : undefined} onSubmit={submit}>
        <h1>{title}</h1>

        <div class="form-grid">
          <div class="field">
            <span>Familiar</span>
            <AgentSelect label="Familiar" agents={props.agents} value={agentId()} onChange={setAgentId} />
          </div>

          <label class="field">
            <span>Name</span>
            <input
              value={name()}
              onInput={(e) => setName(e.currentTarget.value)}
              placeholder="Morning summary"
              autofocus
              required
              maxlength={60}
            />
          </label>

          <label class="field wide">
            <span>Instructions</span>
            <textarea
              value={prompt()}
              onInput={(e) => setPrompt(e.currentTarget.value)}
              placeholder="Summarize what changed in the repo since yesterday and list anything that needs my attention."
              rows={4}
              required
            />
            <p class="hint">Grimoire sends this message to the familiar on each run.</p>
          </label>

          <div class="field wide">
            <span>Repeat</span>
            <Segmented
              label="Repeat"
              value={repeat()}
              options={[
                { value: "daily", label: "Daily" },
                { value: "interval", label: "Every few hours" },
              ]}
              onChange={setRepeat}
            />

            <Show
              when={repeat() === "daily"}
              fallback={
                <div class="routine-schedule">
                  <span class="routine-inline-label">Every</span>
                  <input
                    class="routine-amount"
                    type="number"
                    inputmode="numeric"
                    aria-label={unit() === "hours" ? "Number of hours" : "Number of minutes"}
                    min={unit() === "hours" ? 1 : MIN_MINUTES}
                    max={unit() === "hours" ? MAX_MINUTES / 60 : MAX_MINUTES}
                    step={1}
                    value={amount()}
                    onInput={(e) => setAmount(e.currentTarget.value)}
                  />
                  <Segmented
                    label="Unit"
                    value={unit()}
                    options={[
                      { value: "minutes", label: "Minutes" },
                      { value: "hours", label: "Hours" },
                    ]}
                    onChange={setUnit}
                  />
                </div>
              }
            >
              <div class="routine-schedule">
                <span class="routine-inline-label">At</span>
                <input class="routine-time" type="time" aria-label="Time" value={time()} onInput={(e) => setTime(e.currentTarget.value)} />
              </div>
              <div class="routine-schedule">
                <div class="routine-days" role="group" aria-label="Days">
                  <For each={ALL_DAYS}>
                    {(day) => (
                      <button
                        type="button"
                        class="routine-day"
                        aria-label={DAY_NAMES[day - 1]}
                        aria-pressed={days().includes(day) ? "true" : "false"}
                        onClick={() => toggleDay(day)}
                      >
                        {DAY_NAMES[day - 1][0]}
                      </button>
                    )}
                  </For>
                </div>
                <button type="button" class="ghost small" aria-pressed={sameDays(WEEKDAYS) ? "true" : "false"} onClick={() => setDays(WEEKDAYS)}>
                  Weekdays
                </button>
                <button type="button" class="ghost small" aria-pressed={sameDays(WEEKENDS) ? "true" : "false"} onClick={() => setDays(WEEKENDS)}>
                  Weekends
                </button>
                <button type="button" class="ghost small" aria-pressed={sameDays(ALL_DAYS) ? "true" : "false"} onClick={() => setDays(ALL_DAYS)}>
                  Every day
                </button>
              </div>
            </Show>

            <p class="hint" aria-live="polite">
              {schedule() ? runsText(schedule()!) : repeat() === "interval" ? "Pick between 5 minutes and 7 days." : "Pick a time."}
            </p>
          </div>
        </div>

        <Show when={error()}>
          <p class="error-box" role="alert">
            {error()}
          </p>
        </Show>

        <div class="form-actions">
          <Show when={props.routine}>
            <button type="button" class="ghost danger" onClick={() => void remove()}>
              {confirmDelete() ? "Click again to delete" : "Delete ritual"}
            </button>
          </Show>
          <span class="spacer" />
          <button type="button" class="ghost" onClick={props.onClose}>
            Cancel
          </button>
          <button class="primary" type="submit" disabled={!canSave()}>
            Save ritual
          </button>
        </div>
      </form>
    </Sheet>
  );
}
