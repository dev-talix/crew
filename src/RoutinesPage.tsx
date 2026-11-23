import { createMemo, createSignal, For, onCleanup, onSettled, Show } from "solid-js";
import BotAvatar from "./BotAvatar";
import { AgentSelect, EmptyState, Page, Segmented, Sheet, Toggle } from "./ui";
import { RitualArt, RITUAL_TEMPLATES, type RitualTemplate } from "./EmptyArt";
import { PlusIcon } from "./icons";
import { inkFor } from "./avatar";
import {
  api,
  describeSchedule,
  errorText,
  onEvent,
  relativeTime,
  shortPath,
  type Agent,
  type Routine,
  type Schedule,
  type TriggerState,
  type TriggerStatus,
  type Workflow,
} from "./api";
import { ritualAgents, ritualDoes } from "./spells";
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
type Trigger = "schedule" | "githubPr" | "file" | "webhook" | "email";
type Target = "familiar" | "spell";
type Unit = "minutes" | "hours";

const REPO = /^[A-Za-z0-9-]{1,39}\/[A-Za-z0-9._-]{1,100}$/;

function agentStyle(agent: Agent) {
  return { "--agent": agent.avatar.color, "--agent-ink": inkFor(agent.avatar.color) };
}

function runsText(schedule: Schedule): string {
  const text = describeSchedule(schedule);
  return /^(Every|Weekdays|Weekends|When)/.test(text) ? `Runs ${text[0].toLowerCase()}${text.slice(1)}` : `Runs on ${text}`;
}

const isEvent = (schedule: Schedule) => schedule.kind !== "interval" && schedule.kind !== "daily";

export default function RoutinesPage(props: { agents: Agent[]; onOpenAgent: (id: string) => void }) {
  const [routines, setRoutines] = createSignal<Routine[]>([]);
  const [spells, setSpells] = createSignal<Workflow[]>([]);
  const [loaded, setLoaded] = createSignal(false);
  const [error, setError] = createSignal("");
  const [editing, setEditing] = createSignal<Routine | "new" | { template: RitualTemplate } | null>(null);
  const [now, setNow] = createSignal(Date.now());
  const [skips, setSkips] = createSignal<RitualSkip[]>([]);
  const [triggers, setTriggers] = createSignal<TriggerStatus | null>(null);
  const loadTriggers = () => void api.triggerStatus().then(setTriggers, () => undefined);

  const load = async () => {
    try {
      void api.ritualSkips().then(setSkips, () => undefined);
      loadTriggers();
      const [list, spellList] = await Promise.all([api.listRoutines(), api.listWorkflows()]);
      setRoutines(list);
      setSpells(spellList);
      setError("");
    } catch (err) {
      setError(errorText(err));
    } finally {
      setLoaded(true);
    }
  };

  onSettled(() => void load());
  const unlisten = onEvent("routines_changed", () => void load());
  // Deleting a spell deletes the rituals that cast it.
  const unlistenSpells = onEvent("workflows_changed", () => void load());
  const tick = window.setInterval(() => {
    setNow(Date.now());
    // Watchers check quietly, so pick up their last check here.
    loadTriggers();
  }, 60_000);
  onCleanup(() => {
    clearInterval(tick);
    void unlisten.then((stop) => stop());
    void unlistenSpells.then((stop) => stop());
  });

  const groups = createMemo(() =>
    props.agents
      .map((agent) => ({ agent, routines: routines().filter((r) => r.agentId === agent.id) }))
      .filter((group) => group.routines.length > 0),
  );
  const spellGroups = createMemo(() =>
    spells()
      .map((spell) => ({ spell, routines: routines().filter((r) => r.workflowId === spell.id) }))
      .filter((group) => group.routines.length > 0),
  );
  const agentById = (id: string) => props.agents.find((a) => a.id === id);

  const upsert = (saved: Routine) =>
    setRoutines((list) => (list.some((r) => r.id === saved.id) ? list.map((r) => (r.id === saved.id ? saved : r)) : [...list, saved]));

  const setEnabled = async (routine: Routine, enabled: boolean) => {
    setError("");
    upsert({ ...routine, enabled });
    try {
      const { id, agentId, workflowId, name, prompt, schedule } = routine;
      upsert(await api.saveRoutine({ id, agentId, workflowId, name, prompt, schedule, enabled }));
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

        <Show when={loaded() && groups().length === 0 && spellGroups().length === 0}>
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
                      does={ritualDoes(routine, spells())}
                      skip={skips().find((s) => s.routineId === routine.id && s.at >= (routine.lastRunAt ?? 0))}
                      trigger={triggers()?.states.find((t) => t.routineId === routine.id)}
                      webhookError={triggers()?.webhookError ?? null}
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

        <For each={spellGroups()}>
          {(group) => (
            <section class="routine-group">
              <div class="routine-group-head routine-spell-head">
                <span class="routine-spell-faces" aria-hidden="true">
                  <For each={ritualAgents(group.routines[0], spells())}>
                    {(id) => <Show when={agentById(id)}>{(a) => <BotAvatar avatar={a().avatar} size={22} />}</Show>}
                  </For>
                </span>
                <span>{group.spell.name}</span>
                <span class="routine-spell-tag">Spell</span>
              </div>
              <div class="list">
                <For each={group.routines}>
                  {(routine) => (
                    <RoutineRow
                      routine={routine}
                      does={ritualDoes(routine, spells())}
                      skip={skips().find((s) => s.routineId === routine.id && s.at >= (routine.lastRunAt ?? 0))}
                      trigger={triggers()?.states.find((t) => t.routineId === routine.id)}
                      webhookError={triggers()?.webhookError ?? null}
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
            spells={spells()}
            webhookBase={triggers()?.webhookBase ?? null}
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
  /** What the ritual does on each run. */
  does: string;
  /** Set when the last run was skipped. */
  skip?: RitualSkip;
  /** How an event ritual's watcher is doing. */
  trigger?: TriggerState;
  /** Why webhooks are off, if they are. */
  webhookError: string | null;
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

  const event = () => isEvent(props.routine.schedule);
  const watching = () => {
    const parts: string[] = [];
    const t = props.trigger;
    if (t?.checkedAt && props.routine.schedule.kind !== "webhook") parts.push(`Checked ${relativeTime(t.checkedAt, props.now)}.`);
    if (t?.firedAt) parts.push(`Last ran on its trigger ${relativeTime(t.firedAt, props.now)}.`);
    return parts.join(" ");
  };
  const triggerError = () =>
    props.routine.schedule.kind === "webhook" ? props.webhookError : (props.trigger?.error ?? null);

  const nextRun = () => {
    if (!props.routine.enabled) return "Paused";
    if (event()) return "Watching";
    const at = props.routine.nextRunAt;
    if (at === null) return "";
    return at <= props.now ? "Running soon" : `Next run ${relativeTime(at, props.now)}`;
  };

  return (
    <div class={["list-row routine-row", { paused: !props.routine.enabled }]}>
      <button type="button" class="list-main routine-open" aria-label={`Edit ${props.routine.name}`} onClick={props.onEdit}>
        <span class="list-title">{props.routine.name}</span>
        <span class="list-sub">{describeSchedule(props.routine.schedule)}</span>
        <span class="list-sub routine-does">{props.does}</span>
        <Show when={event() && props.routine.enabled && watching()}>
          <span class="list-sub routine-watch">{watching()}</span>
        </Show>
        <Show when={event() && props.routine.enabled && triggerError()}>
          {(error) => <span class="routine-skip">{error()}</span>}
        </Show>
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
          label={`Run ${props.routine.name} ${event() ? "on its trigger" : "on schedule"}`}
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
  spells: Workflow[];
  /** Where webhooks arrive; the ritual's secret goes on the end. */
  webhookBase: string | null;
  onClose: () => void;
  onSaved: (routine: Routine) => void;
  onDeleted: (id: string) => void;
}) {
  const seed = props.routine ?? props.template;
  const initial = seed?.schedule;
  const [trigger, setTrigger] = createSignal<Trigger>(!initial || !isEvent(initial) ? "schedule" : (initial.kind as Trigger));
  const [repo, setRepo] = createSignal(initial?.kind === "githubPr" ? initial.repo : "");
  const [path, setPath] = createSignal(initial?.kind === "file" ? shortPath(initial.path) : "");
  const secret = initial?.kind === "webhook" ? initial.secret : "";
  const webhookUrl = () => (secret && props.webhookBase ? `${props.webhookBase}${secret}` : "");
  const [copied, setCopied] = createSignal(false);
  let copiedTimer: number | undefined;
  onCleanup(() => clearTimeout(copiedTimer));
  const copyUrl = async () => {
    try {
      await navigator.clipboard.writeText(webhookUrl());
      setCopied(true);
      clearTimeout(copiedTimer);
      copiedTimer = window.setTimeout(() => setCopied(false), 1500);
    } catch (err) {
      setError(errorText(err));
    }
  };
  const initialMinutes = initial?.kind === "interval" ? initial.minutes : 180;
  const initialUnit: Unit = initialMinutes % 60 === 0 ? "hours" : "minutes";

  const [target, setTarget] = createSignal<Target>(props.routine?.workflowId ? "spell" : "familiar");
  const [agentId, setAgentId] = createSignal(props.routine?.agentId ?? props.agents[0]?.id ?? "");
  const [spellId, setSpellId] = createSignal(props.routine?.workflowId ?? props.spells[0]?.id ?? "");
  const [name, setName] = createSignal(seed?.name ?? "");
  const [prompt, setPrompt] = createSignal(seed?.prompt ?? "");
  const [repeat, setRepeat] = createSignal<Repeat>(initial?.kind === "interval" ? "interval" : "daily");
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

  const spell = () => props.spells.find((s) => s.id === spellId());
  // A spell ritual takes the color of the familiar its spell starts with.
  const agent = () => {
    const id = target() === "spell" ? spell()?.steps[0]?.agentId : agentId();
    return props.agents.find((a) => a.id === id);
  };

  const minutes = () => {
    const value = Number(amount());
    return Number.isInteger(value) ? value * (unit() === "hours" ? 60 : 1) : NaN;
  };
  const intervalValid = () => minutes() >= MIN_MINUTES && minutes() <= MAX_MINUTES;

  const schedule = (): Schedule | null => {
    switch (trigger()) {
      case "githubPr":
        return REPO.test(repo().trim()) ? { kind: "githubPr", repo: repo().trim() } : null;
      case "file":
        return path().trim() ? { kind: "file", path: path().trim() } : null;
      case "webhook":
        return { kind: "webhook", secret };
      case "email":
        return { kind: "email" };
    }
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

  const targetReady = () => (target() === "spell" ? !!spell() : !!agent() && !!prompt().trim());
  const canSave = () => !saving() && targetReady() && !!name().trim() && schedule() !== null;

  const submit = async (e: SubmitEvent) => {
    e.preventDefault();
    const s = schedule();
    if (!s) return;
    setError("");
    setSaving(true);
    try {
      const saved = await api.saveRoutine({
        id: props.routine?.id,
        agentId: target() === "familiar" ? agentId() : null,
        workflowId: target() === "spell" ? spellId() : null,
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
          <div class="field wide">
            <span>Each run</span>
            <Segmented
              label="Each run"
              value={target()}
              options={[
                { value: "familiar", label: "Message a familiar" },
                { value: "spell", label: "Cast a spell" },
              ]}
              onChange={setTarget}
            />
          </div>

          <Show
            when={target() === "spell"}
            fallback={
              <div class="field">
                <span>Familiar</span>
                <AgentSelect label="Familiar" agents={props.agents} value={agentId()} onChange={setAgentId} />
              </div>
            }
          >
            <div class="field">
              <span>Spell</span>
              <Show when={props.spells.length} fallback={<p class="hint routine-no-spells">No spells yet. Make one on the Spells page.</p>}>
                <div class="agent-select">
                  <Show when={agent()}>{(a) => <BotAvatar avatar={a().avatar} size={22} />}</Show>
                  <select aria-label="Spell" value={spellId()} onChange={(e) => setSpellId(e.currentTarget.value)}>
                    <Show when={!spell()}>
                      <option value="" disabled>
                        Choose a spell
                      </option>
                    </Show>
                    <For each={props.spells}>{(s) => <option value={s.id}>{s.name}</option>}</For>
                  </select>
                </div>
              </Show>
            </div>
          </Show>

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
            <span>{target() === "spell" ? "Starting text" : "Instructions"}</span>
            <textarea
              value={prompt()}
              onInput={(e) => setPrompt(e.currentTarget.value)}
              placeholder={
                target() === "spell"
                  ? "Optional"
                  : "Summarize what changed in the repo since yesterday and list anything that needs my attention."
              }
              rows={target() === "spell" ? 3 : 4}
              required={target() === "familiar"}
            />
            <p class="hint">
              {target() === "spell"
                ? "Grimoire casts the spell on each run, with this as its {{input}}. A run is skipped while the spell is still being cast."
                : "Grimoire sends this message to the familiar on each run."}
            </p>
          </label>

          <div class="field wide">
            <span>Runs</span>
            <Segmented
              label="Runs"
              value={trigger()}
              options={[
                { value: "schedule", label: "On a schedule" },
                { value: "githubPr", label: "GitHub PR" },
                { value: "file", label: "File" },
                { value: "webhook", label: "Webhook" },
                { value: "email", label: "Email" },
              ]}
              onChange={setTrigger}
            />
            <Show when={trigger() === "githubPr"}>
              <input
                class="routine-trigger-input"
                aria-label="GitHub repo"
                value={repo()}
                onInput={(e) => setRepo(e.currentTarget.value)}
                placeholder="owner/repo"
                spellcheck={false}
              />
              <p class="hint">Grimoire checks for new pull requests every 3 minutes with the gh command, so gh has to be signed in.</p>
            </Show>
            <Show when={trigger() === "file"}>
              <input
                class="routine-trigger-input"
                aria-label="File or folder"
                value={path()}
                onInput={(e) => setPath(e.currentTarget.value)}
                placeholder="~/Documents/Inbox"
                spellcheck={false}
              />
              <p class="hint">A file or folder in your home folder. Grimoire checks it every 30 seconds and runs when it changes.</p>
            </Show>
            <Show when={trigger() === "webhook"}>
              <Show
                when={webhookUrl()}
                fallback={<p class="hint">Save the ritual to get its webhook address.</p>}
              >
                <div class="routine-webhook">
                  <code>{webhookUrl()}</code>
                  <button type="button" class="ghost small" onClick={() => void copyUrl()}>
                    <span aria-live="polite">{copied() ? "Copied" : "Copy"}</span>
                  </button>
                </div>
                <p class="hint">
                  A POST to this address runs the ritual, with the request body passed along as untrusted text. It only answers on this Mac. Keep the address private.
                </p>
              </Show>
            </Show>
            <Show when={trigger() === "email"}>
              <p class="hint">
                Runs when a new message reaches the inbox in the Mail app. Mail has to be open, and macOS asks once to let Grimoire read it.
              </p>
            </Show>
            <Show when={trigger() !== "schedule"}>
              <p class="hint">An event ritual runs at most once a minute. Saving it never runs it; only something new after that does.</p>
            </Show>
          </div>

          <Show when={trigger() === "schedule"}>
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
          </Show>
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
