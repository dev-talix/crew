import { createEffect, createSignal, For, onCleanup, onSettled, Show } from "solid-js";
import { api, errorText, onEvent, onMountAsync, type Agent, type Helper, type Message } from "./api";
import SideConversation from "./SideConversation";
import { formatDuration } from "./timeline";
import "./helpers.css";

const STATUS: Record<Helper["status"], string> = {
  running: "Running",
  done: "Done",
  failed: "Failed",
  stopped: "Stopped",
  timeout: "Timed out",
  interrupted: "Interrupted",
};

/** The row SideConversation expects, pointing at the helper's hidden chat. */
const transcriptRow = (helper: Helper): Message => ({
  id: 0,
  agentId: helper.agentId,
  role: "side",
  text: `Helper ${helper.label}`,
  status: "done",
  source: helper.chatId,
  createdAt: helper.startedAt,
  updatedAt: null,
});

/**
 * The chat header's helpers button: a familiar's running and recent background helpers, with status,
 * time so far, a Stop button, and each helper's transcript. Hidden until the familiar has started one.
 */
export default function Helpers(props: { agent: Agent }) {
  const [helpers, setHelpers] = createSignal<Helper[]>([]);
  const [open, setOpen] = createSignal(false);
  const [error, setError] = createSignal("");
  const [now, setNow] = createSignal(Date.now());
  const timer = setInterval(() => setNow(Date.now()), 5000);
  onCleanup(() => clearInterval(timer));
  let anchor!: HTMLSpanElement;

  const load = () => {
    const id = props.agent.id;
    api.listHelpers(id).then(
      (list) => id === props.agent.id && setHelpers(list),
      () => {},
    );
  };
  createEffect(
    () => props.agent.id,
    () => {
      setHelpers([]);
      setOpen(false);
      setError("");
      load();
    },
  );
  onMountAsync(async (keep) => keep(await onEvent("helpers_changed", load)));

  onSettled(() => {
    const onPointer = (e: PointerEvent) => {
      if (open() && !anchor.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (open() && e.key === "Escape") {
        e.stopPropagation();
        setOpen(false);
      }
    };
    document.addEventListener("pointerdown", onPointer);
    document.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("pointerdown", onPointer);
      document.removeEventListener("keydown", onKey, true);
    };
  });

  const running = () => helpers().filter((h) => h.status === "running").length;
  const elapsed = (h: Helper) => formatDuration((h.finishedAt ?? now()) - h.startedAt);
  const stop = (h: Helper) => {
    setError("");
    api.stopHelper(h.id).catch((e) => setError(errorText(e)));
  };

  return (
    <Show when={helpers().length > 0}>
      <span class="helpers-anchor" ref={anchor}>
        <button
          class={["helpers-trigger", { working: running() > 0 }]}
          aria-haspopup="dialog"
          aria-expanded={open() ? "true" : "false"}
          title="Background helpers"
          onClick={() => setOpen(!open())}
        >
          {running() > 0 ? `${running()} ${running() === 1 ? "helper" : "helpers"} working` : "Helpers"}
        </button>
        <Show when={open()}>
          <div class="picker helpers-menu" role="dialog" aria-label={`${props.agent.name}'s helpers`}>
            <div class="picker-section">Helpers</div>
            <Show when={error()}>
              <p class="helpers-error">{error()}</p>
            </Show>
            <div class="picker-list">
              {/* Keyed by id, so a refresh keeps an open transcript open. */}
              <For each={helpers()} keyed={(h: Helper) => h.id}>
                {(h) => (
                  <div class="helper-item">
                    <div class="helper-meta">
                      <span class={["helper-status", h().status]}>{STATUS[h().status]}</span>
                      <span class="helper-time">{elapsed(h())}</span>
                      <Show when={h().status === "running"}>
                        <button class="helper-stop" onClick={() => stop(h())}>
                          Stop
                        </button>
                      </Show>
                    </div>
                    <SideConversation message={transcriptRow(h())} asker={props.agent.name} />
                    <Show when={h().status !== "running" && h().status !== "done" && h().result}>
                      {(result) => <p class="helper-note">{result()}</p>}
                    </Show>
                  </div>
                )}
              </For>
            </div>
          </div>
        </Show>
      </span>
    </Show>
  );
}
