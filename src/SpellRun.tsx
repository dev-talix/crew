import { createSignal, For, onSettled, Show } from "solid-js";
import BotAvatar from "./BotAvatar";
import { api, errorText, onEvent, onMountAsync, relativeTime, type Agent, type RunState, type Workflow, type WorkflowProgress, type WorkflowRunDetail } from "./api";
import { duration, isLong } from "./spells";

export const STATE_LABELS: Record<RunState, string> = { running: "Casting", done: "Finished", failed: "Failed", stopped: "Stopped" };

/** Text that starts clamped when it's long, with a button to show all of it. */
function TextBlock(props: { label: string; text: string }) {
  const [open, setOpen] = createSignal(false);
  const long = () => isLong(props.text);
  return (
    <div class="run-text">
      <div class="run-text-head">
        <span>{props.label}</span>
        <Show when={long()}>
          <button type="button" aria-expanded={open() ? "true" : "false"} onClick={() => setOpen(!open())}>
            {open() ? "Show less" : "Show all"}
          </button>
        </Show>
      </div>
      <p class={["run-text-body", { clamped: long() && !open() }]}>{props.text}</p>
    </div>
  );
}

/** One cast of a spell: its starting text, then each step's prompt and reply. Updates while it runs. */
export default function SpellRun(props: {
  runId: string;
  workflow?: Workflow;
  agents: Agent[];
  /** Whether this spell is being cast right now, by this run or another. */
  busy: boolean;
  onCastAgain: (input: string) => Promise<string>;
  onClose: () => void;
}) {
  const [run, setRun] = createSignal<WorkflowRunDetail | null>(null);
  const [error, setError] = createSignal("");
  const [casting, setCasting] = createSignal(false);
  const [now, setNow] = createSignal(Date.now());
  const agent = (id: string) => props.agents.find((a) => a.id === id);

  const load = async (id = props.runId) => {
    try {
      const detail = await api.getWorkflowRun(id);
      if (id === props.runId) setRun(detail);
    } catch (e) {
      setError(errorText(e));
    }
  };

  onMountAsync(async (keep) => {
    keep(
      await onEvent<WorkflowProgress>("workflow_progress", (p) => {
        if (p.runId === props.runId) void load();
      }),
    );
    await load();
  });
  onSettled(() => {
    const tick = window.setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(tick);
  });

  const took = (start: number, end: number | null) => duration((end ?? now()) - start);

  const castAgain = async () => {
    const current = run();
    if (!current) return;
    setError("");
    setCasting(true);
    try {
      await props.onCastAgain(current.input);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setCasting(false);
    }
  };

  return (
    <div class="run-sheet">
      <header class="run-head">
        <h1>{props.workflow?.name ?? "Deleted spell"}</h1>
        <Show when={run()}>
          {(r) => (
            <p class="run-meta">
              <span class={["run-state", r().state]}>{STATE_LABELS[r().state]}</span>
              <span>
                {r().ritual ? `Cast by the ritual ${r().ritual}` : "Cast by you"} {relativeTime(r().startedAt, now())}
              </span>
              <span>{took(r().startedAt, r().endedAt)}</span>
            </p>
          )}
        </Show>
      </header>

      <Show when={run()} fallback={<Show when={!error()}><p class="hint">Loading…</p></Show>}>
        {(r) => (
          <>
            <Show when={r().input} fallback={<p class="hint">Cast with no starting text.</p>}>
              <TextBlock label="Starting text" text={r().input} />
            </Show>
            <Show when={r().error}>
              <p class="error-box" role="alert">
                {r().error}
              </p>
            </Show>
            <ol class="run-steps" aria-label="Steps">
              <For each={r().steps}>
                {(step) => {
                  const a = () => agent(step.agentId);
                  return (
                    <li class={["run-step", step.state]} style={a() ? { "--agent": a()!.avatar.color } : undefined}>
                      <div class="run-step-head">
                        <Show when={a()} fallback={<span class="wf-missing" aria-hidden="true" />}>
                          {(found) => <BotAvatar avatar={found().avatar} size={22} working={step.state === "running"} />}
                        </Show>
                        <span class="run-step-title">
                          Step {step.step + 1}, {a()?.name ?? "a deleted familiar"}
                        </span>
                        <span class="spacer" />
                        <span class={["run-state", step.state]}>{STATE_LABELS[step.state]}</span>
                        <span class="run-time">{took(step.startedAt, step.endedAt)}</span>
                      </div>
                      <TextBlock label="Sent" text={step.prompt} />
                      <Show when={step.reply !== null}>
                        <TextBlock label="Reply" text={step.reply ?? ""} />
                      </Show>
                      <Show when={step.state === "running"}>
                        <p class="hint">Working on a reply…</p>
                      </Show>
                      <Show when={step.error}>
                        <p class="wf-error">{step.error}</p>
                      </Show>
                      <Show when={step.wentTo !== null}>
                        <p class="run-went">
                          The check matched, so the spell went {step.wentTo! <= step.step ? "back" : "on"} to step {step.wentTo! + 1}.
                        </p>
                      </Show>
                    </li>
                  );
                }}
              </For>
            </ol>
          </>
        )}
      </Show>

      <Show when={error()}>
        <p class="error-box" role="alert">
          {error()}
        </p>
      </Show>

      <div class="form-actions">
        <span class="spacer" />
        <button type="button" class="ghost" onClick={() => props.onClose()}>
          Close
        </button>
        <button
          type="button"
          class="primary"
          disabled={!run() || !props.workflow || props.busy || casting()}
          title={props.busy ? "This spell is being cast" : undefined}
          onClick={() => void castAgain()}
        >
          Cast again
        </button>
      </div>
    </div>
  );
}
