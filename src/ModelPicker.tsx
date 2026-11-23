import { createResource, For, onCleanup, onMount, Show } from "solid-js";
import { PROVIDER_NAMES, type Agent, type ModelOption } from "./api";
import { CheckIcon } from "./icons";
import { effortName, modelCatalog, modelName } from "./models";

export default function ModelPicker(props: {
  agent: Agent;
  onPick: (model: string, effort: string) => void;
  onClose: () => void;
}) {
  const [catalog] = createResource(() => props.agent.provider, modelCatalog);
  let panel!: HTMLDivElement;

  const models = () => catalog() ?? [];
  const selected = () => props.agent.model ?? "";
  const effort = () => props.agent.effort ?? "";
  const current = (): ModelOption | undefined =>
    models().find((m) => m.id === selected()) ?? (selected() ? undefined : models().find((m) => m.isDefault));
  const efforts = () => current()?.efforts ?? ["low", "medium", "high", "xhigh", "max"];
  const defaultNote = () => {
    const active = props.agent.activeModel;
    if (active && !selected()) return `Now ${modelName(active, models())}`;
    const fallback = models().find((m) => m.isDefault);
    return fallback ? `Now ${fallback.name}` : `Your ${PROVIDER_NAMES[props.agent.provider]} setting`;
  };

  const pickModel = (id: string) => {
    const next = models().find((m) => m.id === id);
    const keepEffort = !effort() || !next || next.efforts.includes(effort());
    props.onPick(id, keepEffort ? effort() : "");
  };

  const onPointer = (e: PointerEvent) => {
    if (!panel.contains(e.target as Node)) props.onClose();
  };
  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      props.onClose();
    }
  };
  onMount(() => {
    setTimeout(() => document.addEventListener("pointerdown", onPointer));
    document.addEventListener("keydown", onKey, true);
    panel.querySelector<HTMLButtonElement>("[aria-checked='true']")?.focus();
  });
  onCleanup(() => {
    document.removeEventListener("pointerdown", onPointer);
    document.removeEventListener("keydown", onKey, true);
  });

  const Row = (row: { id: string; name: string; note: string }) => (
    <button
      type="button"
      role="menuitemradio"
      class="picker-item"
      aria-checked={selected() === row.id}
      onClick={() => pickModel(row.id)}
    >
      <span class="picker-text">
        <span class="picker-name">{row.name}</span>
        <Show when={row.note}>
          <span class="picker-note">{row.note}</span>
        </Show>
      </span>
      <Show when={selected() === row.id}>
        <CheckIcon />
      </Show>
    </button>
  );

  return (
    <div class="picker" ref={panel} role="menu" aria-label={`${PROVIDER_NAMES[props.agent.provider]} model`}>
      <div class="picker-section">{PROVIDER_NAMES[props.agent.provider]} model</div>
      <div class="picker-list">
        <Row id="" name="Default" note={defaultNote()} />
        <Show when={!catalog.loading} fallback={<div class="picker-status">Loading models…</div>}>
          <Show when={!catalog.error} fallback={<div class="picker-status">Could not load the model list. Default still works.</div>}>
            <For each={models()}>{(model) => <Row id={model.id} name={model.name} note={model.description} />}</For>
          </Show>
        </Show>
      </div>
      <div class="picker-section">Effort</div>
      <div class="effort-row" role="radiogroup" aria-label="Effort">
        <button type="button" role="radio" aria-checked={!effort()} classList={{ on: !effort() }} onClick={() => props.onPick(selected(), "")}>
          Default
        </button>
        <For each={efforts()}>
          {(level) => (
            <button
              type="button"
              role="radio"
              aria-checked={effort() === level}
              classList={{ on: effort() === level }}
              onClick={() => props.onPick(selected(), level)}
            >
              {effortName(level)}
            </button>
          )}
        </For>
      </div>
    </div>
  );
}
