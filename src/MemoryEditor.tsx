import { createSignal, For, onCleanup, Show } from "solid-js";
import { api, errorText, onEvent, onMountAsync, type Agent, type Coven, type Message } from "./api";
import { CheckIcon, MemoryIcon, PlusIcon, TrashIcon } from "./icons";
import {
  ENTRY_LIMIT,
  entriesSize,
  entryLabel,
  entryProblem,
  lastUpdated,
  MEMORY_KINDS,
  MEMORY_LIMIT,
  memoryUsage,
  ruleText,
  type Memory,
  type MemoryEntry,
  type MemoryKind,
  type MemoryScope,
} from "./memory";
import { Sheet } from "./ui";
import "./memory.css";

/** An entry being edited. `key` tells rows apart; `saved` is the entry it started from, if any. */
interface Row {
  key: number;
  saved?: MemoryEntry;
  kind: MemoryKind;
  project: string;
  text: string;
}

let nextKey = 0;
const toRow = (entry: MemoryEntry): Row => ({ key: nextKey++, saved: entry, kind: entry.kind, project: entry.project ?? "", text: entry.text });
const changed = (row: Row) => !row.saved || row.kind !== row.saved.kind || row.text !== row.saved.text || (row.kind === "project" && row.project !== (row.saved.project ?? ""));

/**
 * Loads a memory and keeps an edited copy of its entries. A change a familiar makes while it's open
 * shows up, unless the user has started editing. With no id yet (a coven being created) it starts empty.
 */
export function createMemory(scope: MemoryScope, id: () => string | null) {
  const [saved, setSaved] = createSignal<Memory | null>(null);
  const [rows, setRows] = createSignal<Row[]>([]);
  const [error, setError] = createSignal("");
  const removed = () => (saved()?.entries ?? []).filter((e) => !rows().some((r) => r.saved?.id === e.id)).map((e) => e.id);
  const dirty = () => removed().length > 0 || rows().some(changed);
  const load = async () => {
    const current = id();
    if (!current) return;
    try {
      const memory = await api.getMemory(scope, current);
      const untouched = !dirty();
      setSaved(memory);
      if (untouched) setRows(memory.entries.map(toRow));
    } catch (err) {
      setError(errorText(err));
    }
  };
  onMountAsync(async (keep) => {
    keep(await onEvent("memory_changed", () => void load()));
    await load();
  });
  const update = (key: number, change: Partial<Row>) => setRows((list) => list.map((r) => (r.key === key ? { ...r, ...change } : r)));
  return {
    saved,
    rows,
    error,
    dirty,
    add: (kind: MemoryKind) => setRows((list) => [...list, { key: nextKey++, kind, project: "", text: "" }]),
    update,
    remove: (key: number) => setRows((list) => list.filter((r) => r.key !== key)),
    usage: () => memoryUsage(entriesSize(rows()), saved()?.limit ?? MEMORY_LIMIT),
    /** Whether every entry would pass the backend's checks. */
    valid: () => rows().every((r) => !entryProblem(r, saved()?.entryLimit ?? ENTRY_LIMIT)),
    /** Saves the edits, to `target` when the owner was only just created. */
    save: async (target = id()) => {
      if (!target) return;
      const drafts = rows().map((r) => ({ id: r.saved?.id, kind: r.kind, project: r.kind === "project" ? r.project : undefined, text: r.text }));
      const memory = await api.saveMemory(scope, target, drafts, removed());
      setSaved(memory);
      setRows(memory.entries.map(toRow));
    },
  };
}

export type MemoryState = ReturnType<typeof createMemory>;

function EntryRow(props: { row: Row; memory: MemoryState; agents: Agent[] }) {
  const problem = () => (props.row.text || props.row.saved ? entryProblem(props.row, props.memory.saved()?.entryLimit ?? ENTRY_LIMIT) : null);
  return (
    <li class="memory-entry">
      <textarea
        class="memory-entry-text"
        rows={2}
        spellcheck={false}
        value={props.row.text}
        aria-label="Entry"
        aria-invalid={problem() ? "true" : "false"}
        placeholder={props.row.kind === "rule" ? "Always answer in British English" : props.row.kind === "fact" ? "Lives in Lisbon" : "The staging deploy runs on Fridays"}
        onInput={(e) => props.memory.update(props.row.key, { text: e.currentTarget.value })}
      />
      <div class="memory-entry-meta">
        <select aria-label="Kind" value={props.row.kind} onChange={(e) => props.memory.update(props.row.key, { kind: e.currentTarget.value as MemoryKind })}>
          <option value="rule">Rule</option>
          <option value="fact">Fact</option>
          <option value="project">Project</option>
        </select>
        <Show when={props.row.kind === "project"}>
          <input
            class="memory-entry-project"
            aria-label="Project"
            maxlength={60}
            placeholder="Project name"
            value={props.row.project}
            onInput={(e) => props.memory.update(props.row.key, { project: e.currentTarget.value })}
          />
        </Show>
        <span class="memory-entry-date">{props.row.saved ? entryLabel(props.row.saved, props.agents) : "New"}</span>
        <span class="spacer" />
        <Show when={problem()}>{(why) => <span class="memory-entry-problem">{why()}</span>}</Show>
        <button type="button" class="mini-button" aria-label="Delete entry" title="Delete entry" onClick={() => props.memory.remove(props.row.key)}>
          <TrashIcon />
        </button>
      </div>
    </li>
  );
}

/** The memory's entries in Rules, Facts, and Projects, with its size against the limit and who last changed it. */
export function MemoryField(props: { memory: MemoryState; agents: Agent[]; label: string; hint: string }) {
  const usage = () => props.memory.usage();
  return (
    <div class="field memory-field">
      <span>{props.label}</span>
      <For each={MEMORY_KINDS}>
        {(section) => {
          const rows = () => props.memory.rows().filter((r) => r.kind === section.kind);
          return (
            <section class="memory-section" aria-label={section.title}>
              <header class="memory-section-head">
                <h2>{section.title}</h2>
                <button type="button" class="ghost memory-add" onClick={() => props.memory.add(section.kind)}>
                  <PlusIcon />
                  Add
                </button>
              </header>
              <Show when={rows().length} fallback={<p class="hint">{section.empty}</p>}>
                <ul class="memory-entries">
                  <For each={rows()} keyed={(r: Row) => r.key}>
                    {(row) => <EntryRow row={row()} memory={props.memory} agents={props.agents} />}
                  </For>
                </ul>
              </Show>
            </section>
          );
        }}
      </For>
      <span class="memory-meta">
        <span>{props.memory.saved() ? lastUpdated(props.memory.saved()!, props.agents) : "Nothing saved yet"}</span>
        <span class={["memory-usage", { danger: usage().over }]}>{usage().label}</span>
      </span>
      <p class="hint">{props.hint}</p>
    </div>
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
          label="What it remembers"
          hint={`${props.agent.name} reads this at the start of every new chat, rules first, and adds to it as it learns. Edits reach it in its next new chat.`}
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
          <button class="primary" type="submit" disabled={saving() || !memory.dirty() || memory.usage().over || !memory.valid()}>
            Save memory
          </button>
        </div>
      </form>
    </Sheet>
  );
}

/** "Make this a rule" on a chat message: saves its text as a rule in the familiar's memory. */
export function RuleButton(props: { message: Message }) {
  const [state, setState] = createSignal<"idle" | "saved" | string>("idle");
  let timer: number | undefined;
  onCleanup(() => clearTimeout(timer));
  const save = async () => {
    try {
      await api.addMemory("familiar", props.message.agentId, "rule", null, ruleText(props.message.text));
      setState("saved");
    } catch (err) {
      setState(errorText(err));
    }
    clearTimeout(timer);
    timer = window.setTimeout(() => setState("idle"), 2500);
  };
  const label = () => (state() === "idle" ? "Make this a rule" : state() === "saved" ? "Saved as a rule" : state());
  return (
    <button type="button" class="mini-button" aria-label={label()} title={label()} onClick={() => void save()}>
      {state() === "saved" ? <CheckIcon /> : <MemoryIcon />}
    </button>
  );
}
