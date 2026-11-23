import { onCleanup } from "solid-js";
import { createStore } from "solid-js/store";
import type { Message } from "./api";
import type { Mood } from "./BotAvatar";

const HAPPY_MS = 2600;
const CONCERNED_MS = 3600;

export function createMoods() {
  const [flash, setFlash] = createStore<Record<string, Mood | undefined>>({});
  const timers = new Map<string, number>();
  onCleanup(() => timers.forEach(clearTimeout));

  const show = (agentId: string, mood: Mood, ms: number) => {
    clearTimeout(timers.get(agentId));
    setFlash(agentId, mood);
    timers.set(
      agentId,
      window.setTimeout(() => setFlash(agentId, undefined), ms),
    );
  };

  return {
    mood: (agentId: string, busy: boolean): Mood => (busy ? "working" : (flash[agentId] ?? "idle")),
    onMessage: (message: Message) => {
      if (message.role === "error") show(message.agentId, "concerned", CONCERNED_MS);
    },
    onStatus: (agentId: string, busy: boolean) => {
      if (busy) setFlash(agentId, undefined);
      else if (flash[agentId] !== "concerned") show(agentId, "happy", HAPPY_MS);
    },
  };
}
