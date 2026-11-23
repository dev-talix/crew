export type Cue = "blink" | "glance-left" | "glance-right" | "fidget" | "wink";

type Listener = (cue: Cue) => void;

const listeners = new Set<Listener>();
const TICK_MS = 900;
const BLINK_CHANCE = 0.17;
const GLANCE_CHANCE = 0.05;
const FIDGET_CHANCE = 0.045;
const WINK_CHANCE = 0.01;
let timer: number | undefined;

const reducedMotion = () => window.matchMedia("(prefers-reduced-motion: reduce)").matches;

function tick() {
  if (document.hidden || reducedMotion()) return;
  for (const listener of listeners) {
    let roll = Math.random();
    if ((roll -= WINK_CHANCE) < 0) listener("wink");
    else if ((roll -= FIDGET_CHANCE) < 0) listener("fidget");
    else if ((roll -= GLANCE_CHANCE) < 0) listener(Math.random() < 0.5 ? "glance-left" : "glance-right");
    else if ((roll -= BLINK_CHANCE) < 0) listener("blink");
  }
}

export function onCue(listener: Listener): () => void {
  listeners.add(listener);
  if (timer === undefined) timer = window.setInterval(tick, TICK_MS);
  return () => {
    listeners.delete(listener);
    if (!listeners.size && timer !== undefined) {
      clearInterval(timer);
      timer = undefined;
    }
  };
}
