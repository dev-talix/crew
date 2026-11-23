import { createSignal, Show } from "solid-js";
import { api, errorText, onEvent, onMountAsync, type Agent, type Coven } from "./api";
import { lastUpdated, MEMORY_LIMIT, memoryUsage, type Memory, type MemoryScope } from "./memory";
import { Sheet } from "./ui";
import "./memory.css";

/**
 * Loads a memory and keeps an edited copy. A change a familiar makes while it's open shows up,
 * unless the user has started editing. With no id yet (a coven being created) it starts empty.
 */
export function createMemory(scope: MemoryScope, id: () => string | null) {
  const [saved, setSaved] = createSignal<Memory | null>(null);
  const [text, setText] = createSignal("");
  const [error, setError] = createSignal("");
  const load = async () => {
    const current = id();
    if (!current) return;
    try {
      const memory = await api.getMemory(scope, current);
      const untouched = text() === (saved()?.text ?? "");
      setSaved(memory);
      if (untouched) setText(memory.text);
    } catch (err) {
      setError(errorText(err));
    }
  };
  onMountAsync(async (keep) => {
    keep(await onEvent("memory_changed", () => void load()));
    await load();
  });
  return {
    saved,
    text,
    setText,
    error,
    dirty: () => text() !== (saved()?.text ?? ""),
    usage: () => memoryUsage(text(), saved()?.limit ?? MEMORY_LIMIT),
    /** Saves the edited text, to `target` when the owner was only just created. */
    save: async (target = id()) => {
      if (!target) return;
      const memory = await api.saveMemory(scope, target, text());
      setSaved(memory);
      setText(memory.text);
    },
  };
}

export type MemoryState = ReturnType<typeof createMemory>;

/** The memory textarea with its size against the limit and who last changed it. */
export function MemoryField(props: { memory: MemoryState; agents: Agent[]; label: string; hint: string; rows?: number }) {
  const usage = () => props.memory.usage();
  return (
    <label class="field memory-field">
      <span>{props.label}</span>
      <textarea
        class="memory-text"
        rows={props.rows ?? 14}
        spellcheck={false}
        value={props.memory.text()}
        placeholder={"- Prefers short answers\n- The staging deploy runs on Fridays"}
        aria-invalid={usage().over ? "true" : "false"}
        onInput={(e) => props.memory.setText(e.currentTarget.value)}
      />
      <span class="memory-meta">
        <span>{props.memory.saved() ? lastUpdated(props.memory.saved()!, props.agents) : "Nothing saved yet"}</span>
        <span class={["memory-usage", { danger: usage().over }]}>{usage().label}</span>
      </span>
      <p class="hint">{props.hint}</p>
    </label>
  );
}

/** The Memory sheet a familiar's chat header opens. */
export function MemorySheet(props: { agent: Agent; agents: Agent[]; covens: Coven[]; onClose: () => void }) {
  const memory = createMemory("familiar", () => props.agent.id);
  const [saving, setSaving] = createSignal(false);
  const [error, setError] = createSignal("");
  const coven = () => props.covens.find((c) => c.id === props.agent.covenId);

  const submit = async (e: Event) => {
    e.preventDefault();
    setSaving(true);
    setError("");
    try {
      await memory.save();
      props.onClose();
    } catch (err) {
      setError(errorText(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Sheet label={`${props.agent.name}'s memory`} wide onClose={props.onClose}>
      <form class="wf-form" onSubmit={submit}>
        <h1>{props.agent.name}'s memory</h1>
        <MemoryField
          memory={memory}
          agents={props.agents}
          label="Notes"
          hint={`${props.agent.name} reads this at the start of every new chat and adds to it as it learns. Edits reach it in its next new chat.`}
        />
        <Show when={coven()}>
          {(c) => <p class="hint">It also reads the memory of the {c().name} coven. Edit that from Home, with the coven's Edit button.</p>}
        </Show>
        <Show when={error() || memory.error()}>
          <p class="error-box" role="alert">
            {error() || memory.error()}
          </p>
        </Show>
        <div class="form-actions">
          <span class="spacer" />
          <button type="button" class="ghost" onClick={props.onClose}>
            Cancel
          </button>
          <button class="primary" type="submit" disabled={saving() || !memory.dirty() || memory.usage().over}>
            Save memory
          </button>
        </div>
      </form>
    </Sheet>
  );
}
