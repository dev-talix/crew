import { createSignal, For, onSettled, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import BotAvatar from "./BotAvatar";
import { hostState, type Agent } from "./api";
import { CloseIcon } from "./icons";
import { homeTitle } from "./linked";
import { CHAT_MIN, clampPanelWidth, OVERLAY_GAP, PANEL_MIN, panelMax, parsePanelWidth } from "./panel";

/** The Mac a remote or linked familiar runs on, and whether that Mac is offline or still owes it its setup. */
export function HostBadge(props: { agent: Agent; host: string; title?: string }) {
  const state = () => hostState(props.agent);
  const title = () => {
    if (props.agent.home) return homeTitle(props.agent);
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

const PANEL_KEY = "grimoire-panel-width";

function readWidth(): string | null {
  try {
    return localStorage.getItem(PANEL_KEY);
  } catch {
    return null;
  }
}

/**
 * A resizable panel on the right of its parent, beside the page rather than over it. A narrow parent
 * gets it floating over the page instead. Escape closes it while focus is inside, and closing hands
 * focus back to `opener`, or to whatever had focus when it opened.
 */
export function SidePanel(props: { label: string; onClose: () => void; children: JSX.Element; opener?: HTMLElement }) {
  let panel!: HTMLDivElement;
  const opener = props.opener ?? (document.activeElement as HTMLElement | null);
  // `wanted` is the width the user chose. `width` is what fits the current layout.
  let wanted = parsePanelWidth(readWidth());
  const [width, setWidth] = createSignal(wanted);
  const [max, setMax] = createSignal(width());
  // What the page beside or under the panel keeps, read from the layout the stylesheet picked.
  const reserve = () => (getComputedStyle(panel).position === "absolute" ? OVERLAY_GAP : CHAT_MIN);
  const available = () => panel.parentElement?.clientWidth ?? width() + CHAT_MIN;
  const fit = (next: number) => {
    setMax(panelMax(available(), reserve()));
    const fitted = clampPanelWidth(next, available(), reserve());
    setWidth(fitted);
    return fitted;
  };
  const resize = (next: number) => {
    const fitted = fit(next);
    wanted = fitted;
    try {
      localStorage.setItem(PANEL_KEY, String(fitted));
    } catch {
      // The width still applies until the panel closes.
    }
  };
  // The handle is on the left edge, so dragging left makes the panel wider.
  const startDrag = (e: PointerEvent) => {
    if (e.button !== 0) return;
    e.preventDefault();
    const handle = e.currentTarget as HTMLElement;
    const startX = e.clientX;
    const startWidth = width();
    handle.setPointerCapture(e.pointerId);
    const move = (m: PointerEvent) => resize(startWidth + startX - m.clientX);
    const stop = () => {
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", stop);
      handle.removeEventListener("pointercancel", stop);
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", stop);
    handle.addEventListener("pointercancel", stop);
  };
  const resizeKey = (e: KeyboardEvent) => {
    const step = e.shiftKey ? 64 : 16;
    if (e.key === "ArrowLeft") resize(width() + step);
    else if (e.key === "ArrowRight") resize(width() - step);
    else return;
    e.preventDefault();
  };
  const onKey = (e: KeyboardEvent) => {
    if (e.key !== "Escape" || e.defaultPrevented) return;
    // A modal opened on top, like Settings from ⌘, while a field here had focus, takes Escape first.
    if (document.querySelector('[aria-modal="true"]')) return;
    e.preventDefault();
    props.onClose();
  };
  onSettled(() => {
    panel.focus();
    // The window or the layout changed: keep the chosen width, but show what fits now.
    const observer = new ResizeObserver(() => fit(wanted));
    if (panel.parentElement) observer.observe(panel.parentElement);
    return () => {
      observer.disconnect();
      if (opener?.isConnected) opener.focus();
    };
  });
  return (
    <div
      ref={panel}
      class="side-panel"
      role="dialog"
      aria-modal="false"
      aria-label={props.label}
      tabindex="-1"
      style={{ "--panel-width": `${width()}px` }}
      onKeyDown={onKey}
    >
      <div
        class="side-panel-handle"
        role="separator"
        aria-orientation="vertical"
        aria-label={`Resize ${props.label.toLowerCase()}`}
        aria-valuenow={width()}
        aria-valuemin={PANEL_MIN}
        aria-valuemax={max()}
        tabindex="0"
        title="Drag to resize. Arrow keys work too."
        onPointerDown={startDrag}
        onKeyDown={resizeKey}
      />
      <button class="icon-button side-panel-close" aria-label={`Close ${props.label.toLowerCase()}`} title="Close (Esc)" onClick={() => props.onClose()}>
        <CloseIcon />
      </button>
      <div class="side-panel-body">{props.children}</div>
    </div>
  );
}

export function Segmented<T extends string>(props: {
  label: string;
  value: T;
  options: { value: T; label: string; disabled?: boolean }[];
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
            disabled={option.disabled}
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
