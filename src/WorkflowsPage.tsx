import { createEffect, createMemo, createSignal, createStore, flush, For, onCleanup, onSettled, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import BotAvatar from "./BotAvatar";
import { AgentSelect, EmptyState, Page, Sheet } from "./ui";
import { SpellArt, SPELL_TEMPLATES, type SpellTemplate } from "./EmptyArt";
import SpellRun, { STATE_LABELS } from "./SpellRun";
import {
  api,
  errorText,
  onEvent,
  onMountAsync,
  relativeTime,
  type Agent,
  type StepCheck,
  type Workflow,
  type WorkflowProgress,
  type WorkflowRun,
} from "./api";
import { describeCheck, fromDrafts, progressLabel, stepGroups, toDrafts, type DraftStep } from "./spells";
import "./workflows.css";

type Run = Omit<WorkflowProgress, "workflowId">;

function Icon(props: { children: JSX.Element }) {
  return (
    <svg
      width="16"
      height="16"
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      {props.children}
    </svg>
  );
}

const UpIcon = () => (
  <Icon>
    <path d="M8 12.5v-9M4 7.5l4-4 4 4" />
  </Icon>
);

const DownIcon = () => (
  <Icon>
    <path d="M8 3.5v9M4 8.5l4 4 4-4" />
  </Icon>
);

const RemoveIcon = () => (
  <Icon>
    <path d="M4 4l8 8M12 4l-8 8" />
  </Icon>
);

/** A loop arrow, for a check that sends the spell to another step. */
const CheckIcon = () => (
  <Icon>
    <path d="M12.5 6.5A4.5 4.5 0 1 0 12 10.5M12.5 3v3.5H9" />
  </Icon>
);

function runLabel(run: Run): string {
  if (run.state === "done") return "Finished";
  if (run.state === "running") return run.steps.length ? progressLabel(run.steps, run.total) : "Casting";
  const at = progressLabel(run.steps.length ? run.steps : [run.step], run.total);
  return `${run.state === "stopped" ? "Stopped" : "Failed"} at ${at.charAt(0).toLowerCase()}${at.slice(1)}`;
}

const freshRun = (workflow: Workflow): Run => ({
  runId: "",
  step: 0,
  total: workflow.steps.length,
  agentId: null,
  steps: [],
  agents: [],
  state: "running",
  error: null,
});

/** What the sidebar asked for when it opened this page: cast one spell, or start a new one. */
export type SpellIntent = { cast: string } | { create: true };

export default function WorkflowsPage(props: {
  agents: Agent[];
  onOpenAgent: (id: string) => void;
  intent?: SpellIntent | null;
  onIntentDone?: () => void;
}) {
  const [workflows, setWorkflows] = createSignal<Workflow[]>([]);
  const [runs, setRuns] = createStore<Record<string, Run | undefined>>({});
  const [history, setHistory] = createSignal<WorkflowRun[]>([]);
  const [loaded, setLoaded] = createSignal(false);
  const [error, setError] = createSignal("");
  const [editing, setEditing] = createSignal<Workflow | "new" | { template: SpellTemplate } | null>(null);
  const [starting, setStarting] = createSignal<Workflow | null>(null);
  const [viewing, setViewing] = createSignal<{ runId: string; workflowId: string } | null>(null);
  const [now, setNow] = createSignal(Date.now());
  const agentById = createMemo(() => new Map(props.agents.map((a) => [a.id, a])));
  const noAgents = () => props.agents.length === 0;

  const setRun = (id: string, run: Run | undefined) =>
    setRuns((runs) => {
      runs[id] = run;
    });

  const loadHistory = () => api.recentWorkflowRuns().then(setHistory, (e) => setError(errorText(e)));

  onMountAsync(async (keep) => {
    try {
      keep(
        await onEvent<WorkflowProgress>("workflow_progress", ({ workflowId, ...run }) => {
          setRun(workflowId, run);
          void loadHistory();
        }),
      );
      keep(await onEvent("workflows_changed", () => api.listWorkflows().then(setWorkflows, (e) => setError(errorText(e)))));
      const [list, running] = await Promise.all([api.listWorkflows(), api.runningWorkflows()]);
      setWorkflows(list);
      for (const id of running) {
        if (runs[id]?.state === "running") continue;
        const workflow = list.find((w) => w.id === id);
        if (workflow) setRun(id, freshRun(workflow));
      }
      await loadHistory();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setLoaded(true);
    }
  });
  onSettled(() => {
    const tick = window.setInterval(() => setNow(Date.now()), 60_000);
    return () => clearInterval(tick);
  });

  const isRunning = (id: string) => runs[id]?.state === "running";
  const castsOf = (id: string) => history().filter((r) => r.workflowId === id);

  // Acts on the sidebar's request once the spells have loaded, and again for each new request.
  createEffect(
    () => (loaded() ? props.intent : null),
    (intent) => {
      if (!intent) return;
      if ("cast" in intent) {
        const spell = workflows().find((w) => w.id === intent.cast);
        if (spell && !isRunning(spell.id)) setStarting(spell);
      } else if (!noAgents()) setEditing("new");
      props.onIntentDone?.();
    },
  );

  const stop = async (id: string) => {
    setError("");
    try {
      await api.stopWorkflow(id);
    } catch (e) {
      setError(errorText(e));
    }
  };

  const run = async (workflow: Workflow, input: string) => {
    const previous = runs[workflow.id] && { ...runs[workflow.id]! };
    // Mark it running before the call. The first progress event can arrive before invoke resolves.
    setRun(workflow.id, freshRun(workflow));
    try {
      const runId = await api.runWorkflow(workflow.id, input);
      setStarting(null);
      void loadHistory();
      return runId;
    } catch (e) {
      setRun(workflow.id, previous);
      throw e;
    }
  };

  const saved = (workflow: Workflow) => {
    setWorkflows((list) =>
      list.some((w) => w.id === workflow.id) ? list.map((w) => (w.id === workflow.id ? workflow : w)) : [...list, workflow],
    );
    setEditing(null);
  };

  const deleted = (id: string) => {
    setWorkflows((list) => list.filter((w) => w.id !== id));
    setHistory((list) => list.filter((r) => r.workflowId !== id));
    setEditing(null);
  };

  const newButton = () => (
    <>
      <Show when={noAgents()}>
        <span class="hint">Summon a familiar first.</span>
      </Show>
      <button class="primary" disabled={noAgents()} onClick={() => setEditing("new")}>
        New spell
      </button>
    </>
  );

  const viewedSpell = () => workflows().find((w) => w.id === viewing()?.workflowId);

  return (
    <>
      <Page title="Spells" actions={<Show when={workflows().length > 0}>{newButton()}</Show>}>
        <Show when={error()}>
          <p class="error-box wf-page-error" role="alert">
            {error()}
          </p>
        </Show>
        <Show when={loaded()}>
          <Show
            when={workflows().length > 0}
            fallback={
              <EmptyState
                art={<SpellArt agents={props.agents} />}
                title="No spells yet"
                body="A spell chains familiars into steps, like research, then write, then review. Each step gets the previous reply."
                action={<div class="wf-empty-action">{newButton()}</div>}
              >
                <Show when={!noAgents()}>
                  <div class="templates">
                    <p class="templates-title">Or start from one of these</p>
                    <For each={SPELL_TEMPLATES}>
                      {(template) => (
                        <button type="button" class="template" onClick={() => setEditing({ template })}>
                          <span class="template-head">
                            <span class="list-title">{template.name}</span>
                            <span class="list-sub">{template.about}</span>
                          </span>
                          <span class="template-body">{template.steps.join(" Then: ")}</span>
                        </button>
                      )}
                    </For>
                  </div>
                </Show>
              </EmptyState>
            }
          >
            <div class="list">
              <For each={workflows()}>
                {(workflow) => {
                  const progress = () => runs[workflow.id];
                  const running = () => isRunning(workflow.id);
                  return (
                    <div class="list-row wf-row">
                      <div class="list-main">
                        <div class="list-title">{workflow.name}</div>
                        <Show when={workflow.createdBy}>
                          {(id) => <div class="list-sub">Made by {agentById().get(id())?.name ?? "a deleted familiar"}</div>}
                        </Show>
                        <div class="wf-meta">
                          <StepChain workflow={workflow} agents={agentById()} progress={progress()} onOpenAgent={props.onOpenAgent} />
                          <span class={["wf-status", { live: running() }]} aria-live="polite">
                            <Show when={progress()}>{(r) => <>{runLabel(r())}</>}</Show>
                          </span>
                        </div>
                        <Show when={progress()?.state === "failed" && progress()?.error}>
                          <p class="wf-error" title={progress()?.error ?? ""}>
                            {progress()?.error}
                          </p>
                        </Show>
                        <Show when={castsOf(workflow.id).length}>
                          <div class="wf-casts" role="group" aria-label={`Recent casts of ${workflow.name}`}>
                            <span class="wf-casts-label">Recent casts</span>
                            <For each={castsOf(workflow.id)}>
                              {(cast) => (
                                <button
                                  type="button"
                                  class={["wf-cast", cast.state]}
                                  title={cast.input ? `Started with: ${cast.input.slice(0, 120)}` : "No starting text"}
                                  onClick={() => setViewing({ runId: cast.id, workflowId: cast.workflowId })}
                                >
                                  <span class="wf-cast-dot" aria-hidden="true" />
                                  {cast.state === "running" ? "Casting now" : `${STATE_LABELS[cast.state]} ${relativeTime(cast.startedAt, now())}`}
                                </button>
                              )}
                            </For>
                          </div>
                        </Show>
                      </div>
                      <div class="list-actions">
                        <Show
                          when={running()}
                          fallback={
                            <button class="ghost small" disabled={noAgents()} onClick={() => setStarting(workflow)}>
                              Cast
                            </button>
                          }
                        >
                          <button class="ghost small" onClick={() => stop(workflow.id)}>
                            Stop
                          </button>
                        </Show>
                        <button class="ghost small" onClick={() => setEditing(workflow)}>
                          Edit
                        </button>
                      </div>
                    </div>
                  );
                }}
              </For>
            </div>
          </Show>
        </Show>
      </Page>

      <Show when={editing()}>
        {(target) => {
          const workflow = typeof target() === "object" && "id" in (target() as object) ? (target() as Workflow) : undefined;
          const template = typeof target() === "object" && "template" in (target() as object) ? (target() as { template: SpellTemplate }).template : undefined;
          return (
            <Sheet label={workflow ? "Edit spell" : "New spell"} wide onClose={() => setEditing(null)}>
              <WorkflowEditor
                workflow={workflow}
                template={template}
                agents={props.agents}
                onSaved={saved}
                onDeleted={deleted}
                onCancel={() => setEditing(null)}
              />
            </Sheet>
          );
        }}
      </Show>

      <Show when={starting()}>
        {(workflow) => (
          <Sheet label={`Cast ${workflow().name}`} onClose={() => setStarting(null)}>
            <RunForm workflow={workflow()} onRun={(input) => run(workflow(), input).then(() => undefined)} onCancel={() => setStarting(null)} />
          </Sheet>
        )}
      </Show>

      <Show when={viewing()}>
        {(viewed) => (
          <Sheet label={`${viewedSpell()?.name ?? "Spell"} cast`} wide onClose={() => setViewing(null)}>
            <SpellRun
              runId={viewed().runId}
              workflow={viewedSpell()}
              agents={props.agents}
              busy={!!viewedSpell() && isRunning(viewedSpell()!.id)}
              onCastAgain={async (input) => {
                const spell = viewedSpell()!;
                const runId = await run(spell, input);
                setViewing({ runId, workflowId: spell.id });
                return runId;
              }}
              onClose={() => setViewing(null)}
            />
          </Sheet>
        )}
      </Show>
    </>
  );
}

/** The spell's steps in order: steps that run together share a pill, and a loop mark shows each check. */
function StepChain(props: { workflow: Workflow; agents: Map<string, Agent>; progress?: Run; onOpenAgent: (id: string) => void }) {
  const groups = () => stepGroups(props.workflow.steps);
  const position = (index: number) => {
    const r = props.progress;
    if (!r || r.state !== "running" || !r.steps.length) return "idle";
    const n = index + 1;
    return r.steps.includes(n) ? "current" : n < Math.min(...r.steps) ? "done" : "pending";
  };
  return (
    <ol class="wf-chain" aria-label="Steps">
      <For each={groups()}>
        {(group, g) => (
          <li>
            <Show when={g() > 0}>
              <span class="wf-connector" aria-hidden="true" />
            </Show>
            <span
              class={["wf-group", { together: group.length > 1 }]}
              title={group.length > 1 ? `Steps ${group.map((i) => i + 1).join(", ")} run together` : undefined}
            >
              <For each={group}>
                {(index) => {
                  const step = props.workflow.steps[index];
                  const agent = () => props.agents.get(step.agentId);
                  return (
                    <Show
                      when={agent()}
                      fallback={
                        <span class="wf-missing" role="img" aria-label={`Step ${index + 1} has no familiar`} title="Familiar deleted" />
                      }
                    >
                      {(a) => (
                        <button
                          class={["wf-chain-agent", { pending: position(index) === "pending" }]}
                          title={`Step ${index + 1}, open ${a().name}`}
                          aria-label={`Open ${a().name}, step ${index + 1}`}
                          style={{ "--agent": a().avatar.color }}
                          onClick={() => props.onOpenAgent(a().id)}
                        >
                          <BotAvatar avatar={a().avatar} size={22} working={position(index) === "current"} />
                        </button>
                      )}
                    </Show>
                  );
                }}
              </For>
            </span>
            <For each={group.filter((i) => props.workflow.steps[i].check)}>
              {(index) => {
                const check = props.workflow.steps[index].check!;
                return (
                  <span class="wf-check-mark" role="img" aria-label={describeCheck(check, index)} title={describeCheck(check, index)}>
                    <CheckIcon />
                    {check.goTo + 1}
                  </span>
                );
              }}
            </For>
          </li>
        )}
      </For>
    </ol>
  );
}

function RunForm(props: { workflow: Workflow; onRun: (input: string) => Promise<void>; onCancel: () => void }) {
  const [input, setInput] = createSignal("");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  // The first steps' {{previous}} is also filled with the starting text.
  const inputUnused = () =>
    !props.workflow.steps.some((s) => s.prompt.includes("{{input}}")) &&
    !(stepGroups(props.workflow.steps)[0] ?? []).some((i) => props.workflow.steps[i].prompt.includes("{{previous}}"));
  let field!: HTMLTextAreaElement;
  onSettled(() => field.focus());

  const submit = async (e?: Event) => {
    e?.preventDefault();
    if (busy()) return;
    setError("");
    setBusy(true);
    try {
      await props.onRun(input());
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form class="wf-form" onSubmit={submit}>
      <h1>Cast {props.workflow.name}</h1>
      <div class="field">
        <label class="field">
          <span>Starting text</span>
          <textarea
            ref={field}
            rows={5}
            value={input()}
            placeholder="Optional"
            onInput={(e) => setInput(e.currentTarget.value)}
            onKeyDown={(e) => e.key === "Enter" && (e.metaKey || e.ctrlKey) && submit(e)}
          />
        </label>
        <Show when={inputUnused()}>
          <p class="hint">
            No step uses <code>{"{{input}}"}</code>, so this text is not sent.
          </p>
        </Show>
      </div>
      <Show when={error()}>
        <p class="error-box" role="alert">
          {error()}
        </p>
      </Show>
      <div class="form-actions">
        <span class="spacer" />
        <button type="button" class="ghost" onClick={() => props.onCancel()}>
          Cancel
        </button>
        <button class="primary" type="submit" disabled={busy()}>
          Cast spell
        </button>
      </div>
    </form>
  );
}

let nextKey = 0;

function WorkflowEditor(props: {
  workflow?: Workflow;
  /** Prefills a new spell; its steps go to the familiars in turn. */
  template?: SpellTemplate;
  agents: Agent[];
  onSaved: (workflow: Workflow) => void;
  onDeleted: (id: string) => void;
  onCancel: () => void;
}) {
  const defaultAgent = (after?: string) => {
    const index = props.agents.findIndex((a) => a.id === after);
    return props.agents[(index + 1) % Math.max(props.agents.length, 1)]?.id ?? "";
  };
  const [name, setName] = createSignal(props.workflow?.name ?? props.template?.name ?? "");
  const fromTemplate = props.template?.steps.map((prompt, i) => ({ agentId: props.agents[i % props.agents.length]?.id ?? "", prompt }));
  const [steps, setSteps] = createStore<DraftStep[]>(
    toDrafts(props.workflow?.steps ?? fromTemplate ?? [{ agentId: defaultAgent(), prompt: "" }], () => nextKey++),
  );
  const [error, setError] = createSignal("");
  const [saving, setSaving] = createSignal(false);
  const [confirmDelete, setConfirmDelete] = createSignal(false);
  let confirmTimer: number | undefined;
  onCleanup(() => clearTimeout(confirmTimer));
  let list!: HTMLOListElement;
  let nameField!: HTMLInputElement;
  onSettled(() => nameField.focus());

  const focusIn = (key: number, selector: string) => {
    flush(); // Apply the step change so the target element exists.
    list.querySelector<HTMLElement>(`[data-step="${key}"] ${selector}`)?.focus();
  };
  const setStep = (index: number, patch: Partial<DraftStep>) =>
    setSteps((draft) => {
      Object.assign(draft[index], patch);
    });
  const setCheck = (index: number, patch: Partial<NonNullable<DraftStep["check"]>>) =>
    setSteps((draft) => {
      Object.assign(draft[index].check!, patch);
    });

  const move = (index: number, by: -1 | 1) => {
    const to = index + by;
    const next = [...steps];
    [next[index], next[to]] = [next[to], next[index]];
    setSteps(() => next);
    // Moving a node drops focus. Keep it on the same button, or on its sibling once that one is disabled.
    const action = by < 0 ? (to === 0 ? "down" : "up") : to === next.length - 1 ? "up" : "down";
    focusIn(next[to].key, `[data-action="${action}"]`);
  };

  const remove = (index: number) => {
    const next = steps.filter((_, i) => i !== index);
    setSteps(() => next);
    focusIn(next[Math.min(index, next.length - 1)].key, "textarea");
  };

  const add = () => {
    const key = nextKey++;
    const agentId = defaultAgent(steps[steps.length - 1]?.agentId);
    setSteps((draft) => {
      draft.push({ key, agentId, prompt: "", parallel: false, check: null });
    });
    focusIn(key, "select");
  };

  const toggleCheck = (index: number) => {
    const step = steps[index];
    if (step.check) {
      setStep(index, { check: null });
      return;
    }
    // Most checks send work back, so start with the step before this one's group.
    const groupStart = (i: number) => {
      while (i > 0 && steps[i].parallel) i--;
      return i;
    };
    const back = index > 0 ? groupStart(groupStart(index) - 1) : 0;
    setStep(index, { check: { when: "lacks", phrase: "", target: steps[back].key } });
    focusIn(step.key, ".wf-check input");
  };

  const submit = async (e: SubmitEvent) => {
    e.preventDefault();
    setError("");
    const list = fromDrafts(steps);
    if (typeof list === "string") {
      setError(list);
      return;
    }
    setSaving(true);
    try {
      const workflow = await api.saveWorkflow({ id: props.workflow?.id, name: name().trim(), steps: list });
      props.onSaved(workflow);
    } catch (err) {
      setError(errorText(err));
    } finally {
      setSaving(false);
    }
  };

  const destroy = async () => {
    const workflow = props.workflow;
    if (!workflow) return;
    if (!confirmDelete()) {
      setConfirmDelete(true);
      confirmTimer = window.setTimeout(() => setConfirmDelete(false), 4000);
      return;
    }
    try {
      await api.deleteWorkflow(workflow.id);
      props.onDeleted(workflow.id);
    } catch (err) {
      setError(errorText(err));
    }
  };

  const agentName = (id: string) => props.agents.find((a) => a.id === id)?.name ?? "no familiar";
  // A check can go to the first step of a group, not to one that runs alongside another.
  const canGoTo = (index: number) => index === 0 || !steps[index].parallel;
  const targetShown = (key: number | null) => steps.some((s, i) => s.key === key && canGoTo(i));

  return (
    <form class="wf-form" onSubmit={submit}>
      <h1>{props.workflow ? "Edit spell" : "New spell"}</h1>
      <label class="field">
        <span>Name</span>
        <input
          ref={nameField}
          value={name()}
          onInput={(e) => setName(e.currentTarget.value)}
          placeholder="Weekly newsletter"
          required
          maxlength={60}
        />
      </label>

      <div class="field">
        <span id="wf-steps-label">Steps</span>
        <p class="hint">
          Use <code>{"{{input}}"}</code> for the starting text and <code>{"{{previous}}"}</code> for the last reply. Without{" "}
          <code>{"{{previous}}"}</code>, the last reply is added at the end. Steps that run together start at once, and the next step
          gets all their replies. A check sends the spell to another step when the reply has, or lacks, a phrase, at most 3 times.
        </p>
        <ol class="wf-steps" ref={list} aria-labelledby="wf-steps-label">
          <For each={steps}>
            {(step, index) => {
              const n = () => index() + 1;
              return (
                <li class={["wf-step", { alongside: step.parallel && index() > 0 }]} data-step={step.key}>
                  <span class="wf-step-num" aria-hidden="true">
                    {n()}
                  </span>
                  <div class="wf-step-body">
                    <div class="wf-step-head">
                      <AgentSelect
                        agents={props.agents}
                        value={step.agentId}
                        label={`Step ${n()} familiar`}
                        onChange={(id) => setStep(index(), { agentId: id })}
                      />
                      <Show when={index() > 0}>
                        <label class="wf-flag" title="Start this step together with the one before it">
                          <input
                            type="checkbox"
                            checked={step.parallel}
                            onChange={(e) => setStep(index(), { parallel: e.currentTarget.checked })}
                          />
                          With step {index()}
                        </label>
                      </Show>
                      <span class="spacer" />
                      <button
                        type="button"
                        class={["icon-button", { on: !!step.check }]}
                        data-action="check"
                        aria-label={step.check ? `Remove step ${n()}'s check` : `Add a check to step ${n()}`}
                        aria-pressed={step.check ? "true" : "false"}
                        title={step.check ? "Remove check" : "Add a check"}
                        onClick={() => toggleCheck(index())}
                      >
                        <CheckIcon />
                      </button>
                      <button
                        type="button"
                        class="icon-button"
                        data-action="up"
                        aria-label={`Move step ${n()} up`}
                        title="Move up"
                        disabled={index() === 0}
                        onClick={() => move(index(), -1)}
                      >
                        <UpIcon />
                      </button>
                      <button
                        type="button"
                        class="icon-button"
                        data-action="down"
                        aria-label={`Move step ${n()} down`}
                        title="Move down"
                        disabled={index() === steps.length - 1}
                        onClick={() => move(index(), 1)}
                      >
                        <DownIcon />
                      </button>
                      <button
                        type="button"
                        class="icon-button"
                        aria-label={`Remove step ${n()}`}
                        title="Remove step"
                        disabled={steps.length === 1}
                        onClick={() => remove(index())}
                      >
                        <RemoveIcon />
                      </button>
                    </div>
                    <textarea
                      class="wf-prompt"
                      aria-label={`Step ${n()} instructions`}
                      rows={3}
                      value={step.prompt}
                      placeholder={index() === 0 ? "Research {{input}} and list the key facts." : "Write a short post from this research."}
                      onInput={(e) => setStep(index(), { prompt: e.currentTarget.value })}
                    />
                    <Show when={step.check}>
                      {(check) => (
                        <div class="wf-check" role="group" aria-label={`Step ${n()} check`}>
                          <span>If the reply</span>
                          <select
                            aria-label="When"
                            value={check().when}
                            onChange={(e) => setCheck(index(), { when: e.currentTarget.value as StepCheck["when"] })}
                          >
                            <option value="lacks">doesn't contain</option>
                            <option value="contains">contains</option>
                          </select>
                          <input
                            aria-label="Phrase"
                            placeholder="APPROVED"
                            value={check().phrase}
                            onInput={(e) => setCheck(index(), { phrase: e.currentTarget.value })}
                          />
                          <span>go to</span>
                          <select
                            aria-label="Go to step"
                            value={targetShown(check().target) ? String(check().target) : ""}
                            onChange={(e) => setCheck(index(), { target: Number(e.currentTarget.value) })}
                          >
                            <Show when={!targetShown(check().target)}>
                              <option value="" disabled>
                                Choose a step
                              </option>
                            </Show>
                            <For each={steps}>
                              {(other, i) => (
                                <Show when={canGoTo(i())}>
                                  <option value={String(other.key)}>
                                    Step {i() + 1}, {agentName(other.agentId)}
                                  </option>
                                </Show>
                              )}
                            </For>
                          </select>
                        </div>
                      )}
                    </Show>
                  </div>
                </li>
              );
            }}
          </For>
        </ol>
        <button type="button" class="ghost small wf-add" onClick={add}>
          Add step
        </button>
      </div>

      <Show when={error()}>
        <p class="error-box" role="alert">
          {error()}
        </p>
      </Show>

      <div class="form-actions">
        <Show when={props.workflow}>
          <button type="button" class="ghost danger" onClick={destroy}>
            {confirmDelete() ? "Click again to delete" : "Delete spell"}
          </button>
        </Show>
        <span class="spacer" />
        <button type="button" class="ghost" onClick={() => props.onCancel()}>
          Cancel
        </button>
        <button class="primary" type="submit" disabled={saving() || !name().trim()}>
          Save spell
        </button>
      </div>
    </form>
  );
}
