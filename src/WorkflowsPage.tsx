import { createMemo, createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import { createStore } from "solid-js/store";
import type { UnlistenFn } from "@tauri-apps/api/event";
import BotAvatar from "./BotAvatar";
import { AgentSelect, EmptyState, Page, Sheet } from "./ui";
import { api, errorText, onEvent, type Agent, type Workflow, type WorkflowProgress } from "./api";
import "./workflows.css";

type Run = Omit<WorkflowProgress, "workflowId">;
type DraftStep = { key: number; agentId: string; prompt: string };

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

function runLabel(run: Run): string {
  if (run.state === "done") return "Finished";
  if (run.state === "running") return run.step > 0 ? `Step ${run.step} of ${run.total}` : "Running";
  return `${run.state === "stopped" ? "Stopped" : "Failed"} at step ${run.step} of ${run.total}`;
}

export default function WorkflowsPage(props: { agents: Agent[]; onOpenAgent: (id: string) => void }) {
  const [workflows, setWorkflows] = createSignal<Workflow[]>([]);
  const [runs, setRuns] = createStore<Record<string, Run | undefined>>({});
  const [loaded, setLoaded] = createSignal(false);
  const [error, setError] = createSignal("");
  const [editing, setEditing] = createSignal<Workflow | "new" | null>(null);
  const [starting, setStarting] = createSignal<Workflow | null>(null);
  const agentById = createMemo(() => new Map(props.agents.map((a) => [a.id, a])));
  const noAgents = () => props.agents.length === 0;

  let unlisten: UnlistenFn | undefined;
  let disposed = false;
  onCleanup(() => {
    disposed = true;
    unlisten?.();
  });

  onMount(async () => {
    try {
      const stop = await onEvent<WorkflowProgress>("workflow_progress", ({ workflowId, ...run }) => setRuns(workflowId, run));
      if (disposed) return stop();
      unlisten = stop;
      const [list, running] = await Promise.all([api.listWorkflows(), api.runningWorkflows()]);
      setWorkflows(list);
      for (const id of running) {
        if (runs[id]?.state === "running") continue;
        const total = list.find((w) => w.id === id)?.steps.length ?? 0;
        setRuns(id, { step: 0, total, agentId: null, state: "running", error: null });
      }
    } catch (e) {
      setError(errorText(e));
    } finally {
      setLoaded(true);
    }
  });

  const isRunning = (id: string) => runs[id]?.state === "running";

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
    setRuns(workflow.id, { step: 0, total: workflow.steps.length, agentId: null, state: "running", error: null });
    try {
      await api.runWorkflow(workflow.id, input);
      setStarting(null);
    } catch (e) {
      setRuns(workflow.id, previous);
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
    setEditing(null);
  };

  const newButton = () => (
    <>
      <Show when={noAgents()}>
        <span class="hint">Create an agent first.</span>
      </Show>
      <button class="primary" disabled={noAgents()} onClick={() => setEditing("new")}>
        New workflow
      </button>
    </>
  );

  return (
    <>
      <Page title="Workflows" actions={<Show when={workflows().length > 0}>{newButton()}</Show>}>
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
                title="No workflows yet"
                body="Chain agents into steps, like research, then write, then review. Each step gets the previous reply."
                action={<div class="wf-empty-action">{newButton()}</div>}
              />
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
                        <div class="wf-meta">
                          <ol class="wf-chain" aria-label="Steps">
                            <For each={workflow.steps}>
                              {(step, index) => {
                                const agent = () => agentById().get(step.agentId);
                                const position = () => {
                                  const r = progress();
                                  if (!r || r.state !== "running" || r.step === 0) return "idle";
                                  const n = index() + 1;
                                  return n < r.step ? "done" : n === r.step ? "current" : "pending";
                                };
                                return (
                                  <li classList={{ pending: position() === "pending" }}>
                                    <Show when={index() > 0}>
                                      <span class="wf-connector" aria-hidden="true" />
                                    </Show>
                                    <Show
                                      when={agent()}
                                      fallback={
                                        <span
                                          class="wf-missing"
                                          role="img"
                                          aria-label={`Step ${index() + 1} has no agent`}
                                          title="Agent deleted"
                                        />
                                      }
                                    >
                                      {(a) => (
                                        <button
                                          class="wf-chain-agent"
                                          title={`Step ${index() + 1}, open ${a().name}`}
                                          aria-label={`Open ${a().name}, step ${index() + 1}`}
                                          style={{ "--agent": a().avatar.color }}
                                          onClick={() => props.onOpenAgent(a().id)}
                                        >
                                          <BotAvatar avatar={a().avatar} size={22} working={position() === "current"} />
                                        </button>
                                      )}
                                    </Show>
                                  </li>
                                );
                              }}
                            </For>
                          </ol>
                          <span class="wf-status" classList={{ live: running() }} aria-live="polite">
                            <Show when={progress()}>{(r) => <>{runLabel(r())}</>}</Show>
                          </span>
                        </div>
                        <Show when={progress()?.state === "failed" && progress()?.error}>
                          <p class="wf-error" title={progress()?.error ?? ""}>
                            {progress()?.error}
                          </p>
                        </Show>
                      </div>
                      <div class="list-actions">
                        <Show
                          when={running()}
                          fallback={
                            <button class="ghost small" disabled={noAgents()} onClick={() => setStarting(workflow)}>
                              Run
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
          const workflow = target() === "new" ? undefined : (target() as Workflow);
          return (
            <Sheet label={workflow ? "Edit workflow" : "New workflow"} wide onClose={() => setEditing(null)}>
              <WorkflowEditor
                workflow={workflow}
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
          <Sheet label={`Run ${workflow().name}`} onClose={() => setStarting(null)}>
            <RunForm workflow={workflow()} onRun={(input) => run(workflow(), input)} onCancel={() => setStarting(null)} />
          </Sheet>
        )}
      </Show>
    </>
  );
}

function RunForm(props: { workflow: Workflow; onRun: (input: string) => Promise<void>; onCancel: () => void }) {
  const [input, setInput] = createSignal("");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  // Step 1's {{previous}} is also filled with the starting text.
  const inputUnused = () =>
    !props.workflow.steps.some((s) => s.prompt.includes("{{input}}")) && !props.workflow.steps[0]?.prompt.includes("{{previous}}");
  let field!: HTMLTextAreaElement;
  onMount(() => field.focus());

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
      <h1>Run {props.workflow.name}</h1>
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
          Start workflow
        </button>
      </div>
    </form>
  );
}

let nextKey = 0;

function WorkflowEditor(props: {
  workflow?: Workflow;
  agents: Agent[];
  onSaved: (workflow: Workflow) => void;
  onDeleted: (id: string) => void;
  onCancel: () => void;
}) {
  const defaultAgent = (after?: string) => {
    const index = props.agents.findIndex((a) => a.id === after);
    return props.agents[(index + 1) % Math.max(props.agents.length, 1)]?.id ?? "";
  };
  const [name, setName] = createSignal(props.workflow?.name ?? "");
  const [steps, setSteps] = createStore<DraftStep[]>(
    (props.workflow?.steps ?? [{ agentId: defaultAgent(), prompt: "" }]).map((s) => ({ ...s, key: nextKey++ })),
  );
  const [error, setError] = createSignal("");
  const [saving, setSaving] = createSignal(false);
  const [confirmDelete, setConfirmDelete] = createSignal(false);
  let confirmTimer: number | undefined;
  onCleanup(() => clearTimeout(confirmTimer));
  let list!: HTMLOListElement;
  let nameField!: HTMLInputElement;
  onMount(() => nameField.focus());

  const focusIn = (key: number, selector: string) =>
    queueMicrotask(() => list.querySelector<HTMLElement>(`[data-step="${key}"] ${selector}`)?.focus());

  const move = (index: number, by: -1 | 1) => {
    const to = index + by;
    const next = [...steps];
    [next[index], next[to]] = [next[to], next[index]];
    setSteps(next);
    // Moving a node drops focus. Keep it on the same button, or on its sibling once that one is disabled.
    const action = by < 0 ? (to === 0 ? "down" : "up") : to === next.length - 1 ? "up" : "down";
    focusIn(next[to].key, `[data-action="${action}"]`);
  };

  const remove = (index: number) => {
    const next = steps.filter((_, i) => i !== index);
    setSteps(next);
    focusIn(next[Math.min(index, next.length - 1)].key, "textarea");
  };

  const add = () => {
    const key = nextKey++;
    setSteps(steps.length, { key, agentId: defaultAgent(steps[steps.length - 1]?.agentId), prompt: "" });
    focusIn(key, "select");
  };

  const submit = async (e: SubmitEvent) => {
    e.preventDefault();
    setError("");
    setSaving(true);
    try {
      const workflow = await api.saveWorkflow({
        id: props.workflow?.id,
        name: name().trim(),
        steps: steps.map((s) => ({ agentId: s.agentId, prompt: s.prompt })),
      });
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

  return (
    <form class="wf-form" onSubmit={submit}>
      <h1>{props.workflow ? "Edit workflow" : "New workflow"}</h1>
      <label class="field">
        <span>Name</span>
        <input
          ref={nameField}
          value={name()}
          onInput={(e) => setName(e.currentTarget.value)}
          placeholder="Weekly newsletter"
          required
          maxLength={60}
        />
      </label>

      <div class="field">
        <span id="wf-steps-label">Steps</span>
        <p class="hint">
          Use <code>{"{{input}}"}</code> for the starting text and <code>{"{{previous}}"}</code> for the last reply. Without{" "}
          <code>{"{{previous}}"}</code>, the last reply is added at the end.
        </p>
        <ol class="wf-steps" ref={list} aria-labelledby="wf-steps-label">
          <For each={steps}>
            {(step, index) => {
              const n = () => index() + 1;
              return (
                <li class="wf-step" data-step={step.key}>
                  <span class="wf-step-num" aria-hidden="true">
                    {n()}
                  </span>
                  <div class="wf-step-body">
                    <div class="wf-step-head">
                      <AgentSelect
                        agents={props.agents}
                        value={step.agentId}
                        label={`Step ${n()} agent`}
                        onChange={(id) => setSteps(index(), "agentId", id)}
                      />
                      <span class="spacer" />
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
                      onInput={(e) => setSteps(index(), "prompt", e.currentTarget.value)}
                    />
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
            {confirmDelete() ? "Click again to delete" : "Delete workflow"}
          </button>
        </Show>
        <span class="spacer" />
        <button type="button" class="ghost" onClick={() => props.onCancel()}>
          Cancel
        </button>
        <button class="primary" type="submit" disabled={saving() || !name().trim()}>
          Save workflow
        </button>
      </div>
    </form>
  );
}
