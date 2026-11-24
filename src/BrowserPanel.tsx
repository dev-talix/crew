import { createEffect, createSignal, onSettled, Show } from "solid-js";
import { api, errorText, onEvent, onMountAsync, type Agent, type BrowserFrame, type BrowserInput, type BrowserStatus } from "./api";
import { keyInput, toPagePoint } from "./browser";
import "./browser.css";

/**
 * The chat header's browser button: a live view of the familiar's own headless browser, with Start and
 * Stop, and a way for the person to take over, click, type, and hand it back.
 */
export default function BrowserPanel(props: { agent: Agent }) {
  const [status, setStatus] = createSignal<BrowserStatus | null>(null);
  const [frame, setFrame] = createSignal<BrowserFrame | null>(null);
  const [open, setOpen] = createSignal(false);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [url, setUrl] = createSignal("");
  let anchor!: HTMLSpanElement;
  let view: HTMLImageElement | undefined;

  const running = () => !!status()?.running;
  const person = () => running() && status()?.control === "person";

  const update = (next: BrowserStatus) => {
    if (next.agentId !== props.agent.id) return;
    setStatus(next);
    if (!next.running) setFrame(null);
  };

  createEffect(
    () => props.agent.id,
    (id) => {
      setStatus(null);
      setFrame(null);
      setOpen(false);
      setError("");
      api.browserStatus(id).then(update, () => {});
      api.browserFrame(id).then((f) => f && f.agentId === props.agent.id && setFrame(f), () => {});
    },
  );
  onMountAsync(async (keep) => {
    keep(await onEvent<BrowserStatus>("browser_changed", update));
    keep(await onEvent<BrowserFrame>("browser_frame", (f) => f.agentId === props.agent.id && setFrame(f)));
    // Again once listening, in case the only frame arrived before the listener did.
    api.browserFrame(props.agent.id).then((f) => f && f.agentId === props.agent.id && !frame() && setFrame(f), () => {});
  });

  onSettled(() => {
    const onPointer = (e: PointerEvent) => {
      if (open() && !anchor.contains(e.target as Node)) setOpen(false);
    };
    // Escape on the live view only leaves it while the person has control.
    const onKey = (e: KeyboardEvent) => {
      if (open() && e.key === "Escape" && !(person() && e.target === view)) {
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

  const run = async (action: () => Promise<unknown>) => {
    setError("");
    setBusy(true);
    try {
      await action();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };
  const id = () => props.agent.id;
  const start = () => run(async () => update(await api.browserStart(id())));
  const stop = () => run(() => api.browserStop(id()));
  const control = (take: boolean) =>
    run(async () => {
      update(await api.browserControl(id(), take));
      if (take) {
        setUrl(status()?.url ?? "");
        view?.focus();
      }
    });
  const go = (e: SubmitEvent) => {
    e.preventDefault();
    const target = url().trim();
    if (target) void run(() => api.browserNavigate(id(), target));
  };

  const send = (input: BrowserInput) => {
    if (person()) api.browserInput(id(), input).catch((e) => setError(errorText(e)));
  };
  const pagePoint = (e: MouseEvent) => {
    const f = frame();
    return f && view ? toPagePoint(e.clientX, e.clientY, view.getBoundingClientRect(), f.width, f.height) : null;
  };

  // Wheel events come fast; send one scroll per animation frame.
  let scroll: { x: number; y: number; deltaY: number } | null = null;
  const onWheel = (e: WheelEvent) => {
    if (!person()) return;
    e.preventDefault();
    const point = pagePoint(e);
    if (!point) return;
    const deltaY = e.deltaMode === 1 ? e.deltaY * 16 : e.deltaY;
    if (scroll) {
      scroll = { ...point, deltaY: scroll.deltaY + deltaY };
      return;
    }
    scroll = { ...point, deltaY };
    requestAnimationFrame(() => {
      if (scroll) send({ kind: "scroll", ...scroll });
      scroll = null;
    });
  };
  const attachView = (el: HTMLImageElement) => {
    view = el;
    el.addEventListener("wheel", onWheel, { passive: false });
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (!person() || e.isComposing) return;
    // Escape leaves the live view, so the keyboard can reach the panel again; a second Escape closes it.
    if (e.key === "Escape") {
      e.preventDefault();
      view?.blur();
      return;
    }
    const input = keyInput(e);
    if (!input) return;
    e.preventDefault();
    e.stopPropagation();
    send(input);
  };
  const onPaste = (e: ClipboardEvent) => {
    const text = e.clipboardData?.getData("text/plain");
    if (!person() || !text) return;
    e.preventDefault();
    send({ kind: "text", text });
  };

  return (
    <span class="browser-anchor" ref={anchor}>
      <button
        class={["helpers-trigger", { working: running() }]}
        aria-haspopup="dialog"
        aria-expanded={open() ? "true" : "false"}
        title={`${props.agent.name}'s browser`}
        onClick={() => setOpen(!open())}
      >
        {person() ? "You're browsing" : "Browser"}
      </button>
      <Show when={open()}>
        <div class="picker browser-menu" role="dialog" aria-label={`${props.agent.name}'s browser`}>
          <div class="picker-section browser-head">
            <span>Browser</span>
            <Show when={running() && status()?.browser}>{(name) => <span class="browser-name">{name()}</span>}</Show>
          </div>
          <Show when={error()}>
            <p class="browser-error">{error()}</p>
          </Show>
          <Show
            when={running()}
            fallback={
              <div class="browser-off">
                <p>
                  Runs in its own headless browser with a profile just for {props.agent.name}. It never opens windows on your screen, and sign-ins
                  stay between sessions.
                </p>
                <button class="primary" disabled={busy() || !status()} onClick={start}>
                  Start
                </button>
              </div>
            }
          >
            <div class="browser-page">
              <span class="browser-title">{status()?.title || "Untitled page"}</span>
              <span class="browser-url">{status()?.url || "about:blank"}</span>
            </div>
            <Show
              when={person()}
              fallback={<p class="browser-note">View only while {props.agent.name} drives. Take over to click and type.</p>}
            >
              <p class="browser-note browser-yours">
                You have control. {props.agent.name} waits until you hand back.
              </p>
              <form class="browser-go" onSubmit={go}>
                <input aria-label="Address" placeholder="Enter a web address" value={url()} onInput={(e) => setUrl(e.currentTarget.value)} />
                <button class="ghost small" type="submit" disabled={busy() || !url().trim()}>
                  Go
                </button>
              </form>
            </Show>
            <div class={["browser-view", { live: person() }]}>
              <Show when={frame()} fallback={<div class="browser-placeholder">Waiting for the page…</div>}>
                {(f) => (
                  <img
                    ref={attachView}
                    src={`data:image/jpeg;base64,${f().data}`}
                    alt={`${props.agent.name}'s browser: ${status()?.title || "page"}`}
                    draggable={false}
                    tabindex={person() ? 0 : undefined}
                    onClick={(e) => {
                      const point = pagePoint(e);
                      if (point) send({ kind: "click", ...point });
                    }}
                    onKeyDown={onKeyDown}
                    onPaste={onPaste}
                  />
                )}
              </Show>
            </div>
            <div class="browser-actions">
              <button class="ghost small" disabled={busy()} onClick={() => control(!person())}>
                {person() ? "Hand back" : "Take over"}
              </button>
              <button class="ghost small" disabled={busy()} onClick={stop}>
                Stop
              </button>
            </div>
          </Show>
        </div>
      </Show>
    </span>
  );
}
