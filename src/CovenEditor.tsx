import { createSignal, For, onCleanup, onSettled, Show } from "solid-js";
import BotAvatar from "./BotAvatar";
import { api, errorText, type Agent, type Coven } from "./api";
import { createMemory, MemoryField } from "./MemoryEditor";
import { ExportButton } from "./ImportExport";

/** Names a coven and picks its familiars. A familiar picked here leaves its old coven. */
export default function CovenEditor(props: {
  coven: Coven | null;
  agents: Agent[];
  covens: Coven[];
  onSaved: (coven: Coven) => void;
  onDeleted: () => void;
  onCancel: () => void;
}) {
  const [name, setName] = createSignal(props.coven?.name ?? "");
  const [members, setMembers] = createSignal<string[]>(props.coven ? props.agents.filter((a) => a.covenId === props.coven!.id).map((a) => a.id) : []);
  const [error, setError] = createSignal("");
  const [saving, setSaving] = createSignal(false);
  const [confirmDelete, setConfirmDelete] = createSignal(false);
  const memory = createMemory("coven", () => props.coven?.id ?? null);
  let confirmTimer: number | undefined;
  onCleanup(() => clearTimeout(confirmTimer));
  let nameField!: HTMLInputElement;
  onSettled(() => nameField.focus());

  const toggle = (id: string) => setMembers((list) => (list.includes(id) ? list.filter((m) => m !== id) : [...list, id]));
  const elsewhere = (agent: Agent) =>
    agent.covenId && agent.covenId !== props.coven?.id ? props.covens.find((c) => c.id === agent.covenId)?.name : undefined;

  const save = async (e: Event) => {
    e.preventDefault();
    setSaving(true);
    setError("");
    try {
      const saved = await api.saveCoven(props.coven?.id ?? null, name(), members());
      if (memory.dirty()) await memory.save(saved.id);
      props.onSaved(saved);
    } catch (err) {
      setError(errorText(err));
    } finally {
      setSaving(false);
    }
  };

  const remove = async () => {
    if (!confirmDelete()) {
      setConfirmDelete(true);
      confirmTimer = window.setTimeout(() => setConfirmDelete(false), 4000);
      return;
    }
    try {
      await api.deleteCoven(props.coven!.id);
      props.onDeleted();
    } catch (err) {
      setError(errorText(err));
    }
  };

  return (
    <form class="coven-editor wf-form" onSubmit={save}>
      <h1>{props.coven ? `Edit ${props.coven.name}` : "New coven"}</h1>
      <label class="field">
        <span>Name</span>
        <input ref={nameField} required maxlength={40} value={name()} placeholder="Research" onInput={(e) => setName(e.currentTarget.value)} />
      </label>
      <div class="field">
        <span>Familiars</span>
        <div class="coven-members">
          <For each={props.agents}>
            {(agent) => (
              <label class={["coven-member", { on: members().includes(agent.id) }]}>
                <input type="checkbox" checked={members().includes(agent.id)} onChange={() => toggle(agent.id)} />
                <BotAvatar avatar={agent.avatar} size={22} alive={false} />
                <span>{agent.name}</span>
                <Show when={elsewhere(agent)}>{(from) => <span class="coven-member-from">in {from()}</span>}</Show>
              </label>
            )}
          </For>
        </div>
        <p class="hint">A familiar sits in one coven. Picking one that's in another coven moves it here.</p>
      </div>
      <MemoryField
        memory={memory}
        agents={props.agents}
        label="Shared memory"
        hint="Every familiar in the coven reads this at the start of a new chat, and any of them can add to it."
      />
      <Show when={error() || memory.error()}>
        <p class="error-box" role="alert">
          {error() || memory.error()}
        </p>
      </Show>
      <div class="form-actions">
        <Show when={props.coven}>
          <button type="button" class="ghost danger" onClick={() => void remove()}>
            {confirmDelete() ? "Click again to delete" : "Delete coven"}
          </button>
          <ExportButton kind="coven" id={props.coven!.id} onError={setError} />
        </Show>
        <span class="spacer" />
        <button type="button" class="ghost" onClick={props.onCancel}>
          Cancel
        </button>
        <button class="primary" type="submit" disabled={saving() || !name().trim() || memory.usage().over || !memory.valid()}>
          {props.coven ? "Save coven" : "Create coven"}
        </button>
      </div>
    </form>
  );
}
