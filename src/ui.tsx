import { For, onSettled, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import BotAvatar from "./BotAvatar";
import { hostState, type Agent } from "./api";

/** The Mac a remote familiar runs on, and whether that Mac is offline or still owes it its setup. */
export function HostBadge(props: { agent: Agent; host: string; title?: string }) {
  const state = () => hostState(props.agent);
  const title = () => {
    if (props.agent.setupPending) return `${props.host} was offline when this familiar was summoned. Grimoire finishes setting it up when that Mac connects.`;
    if (props.agent.offline) return `${props.host} didn't answer the last time Grimoire tried.`;
    return props.title;
  };
  return (
    <span class={["remote-badge", { "remote-waiting": !!state() }]} title={title()}>
      {props.host}
      <Show when={state()}>{(text) => <span class="remote-state"> · {text()}</span>}</Show>
    </span>
  );
}

export function Page(props: { title: string; actions?: JSX.Element; children: JSX.Element }) {
  return (
    <section class="page" data-slot="page">
      <header class="page-header" data-tauri-drag-region>
        <h1 data-tauri-drag-region>{props.title}</h1>
        <div class="page-actions">{props.actions}</div>
      </header>
      <div class="page-body">{props.children}</div>
    </section>
  );
}

export function EmptyState(props: { title: string; body: string; action?: JSX.Element; art?: JSX.Element; children?: JSX.Element }) {
  return (
    <div class="empty-state">
      {props.art}
      <h2>{props.title}</h2>
      <p>{props.body}</p>
      {props.action}
      {props.children}
    </div>
  );
}

export function Sheet(props: { label: string; onClose: () => void; children: JSX.Element; wide?: boolean }) {
  const onKey = (e: KeyboardEvent) => e.key === "Escape" && props.onClose();
  onSettled(() => {
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });
  return (
    <div class="sheet-backdrop" onClick={(e) => e.target === e.currentTarget && props.onClose()}>
      <div class={["sheet", { narrow: !props.wide }]} role="dialog" aria-modal="true" aria-label={props.label}>
        {props.children}
      </div>
    </div>
  );
}

export function Segmented<T extends string>(props: {
  label: string;
  value: T;
  options: { value: T; label: string }[];
  onChange: (value: T) => void;
}) {
  return (
    <div class="segmented" role="radiogroup" aria-label={props.label}>
      <For each={props.options}>
        {(option) => (
          <button
            type="button"
            role="radio"
            aria-checked={props.value === option.value ? "true" : "false"}
            class={{ on: props.value === option.value }}
            onClick={() => props.onChange(option.value)}
          >
            {option.label}
          </button>
        )}
      </For>
    </div>
  );
}

export function Toggle(props: { label: string; checked: boolean; onChange: (checked: boolean) => void; disabled?: boolean }) {
  return (
    <button
      type="button"
      role="switch"
      class="toggle"
      aria-label={props.label}
      aria-checked={props.checked ? "true" : "false"}
      disabled={props.disabled}
      onClick={() => props.onChange(!props.checked)}
    >
      <span />
    </button>
  );
}

export function AgentSelect(props: { agents: Agent[]; value: string; onChange: (id: string) => void; label: string }) {
  const current = () => props.agents.find((a) => a.id === props.value);
  return (
    <div class="agent-select">
      <Show when={current()}>{(agent) => <BotAvatar avatar={agent().avatar} size={22} />}</Show>
      <select aria-label={props.label} value={props.value} onChange={(e) => props.onChange(e.currentTarget.value)}>
        <Show when={!current()}>
          <option value="" disabled>
            Choose a familiar
          </option>
        </Show>
        <For each={props.agents}>{(agent) => <option value={agent.id}>{agent.name}</option>}</For>
      </select>
    </div>
  );
}
