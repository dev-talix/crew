import { createStore, onCleanup } from "solid-js";
import type { Message } from "./api";
import type { Mood } from "./BotAvatar";

const HAPPY_MS = 2600;
const CONCERNED_MS = 3600;

export function createMoods() {
  const [flash, setFlash] = createStore<Record<string, Mood | undefined>>({});
  // Store writes apply on the next microtask, so decisions read this plain copy instead.
  const current = new Map<string, Mood>();
  const timers = new Map<string, number>();
  onCleanup(() => timers.forEach(clearTimeout));

  const set = (agentId: string, mood: Mood | undefined) => {
    if (mood) current.set(agentId, mood);
    else current.delete(agentId);
    setFlash((moods) => {
      moods[agentId] = mood;
    });
  };

  const show = (agentId: string, mood: Mood, ms: number) => {
    clearTimeout(timers.get(agentId));
    set(agentId, mood);
    timers.set(
      agentId,
      window.setTimeout(() => set(agentId, undefined), ms),
    );
  };

  return {
    mood: (agentId: string, busy: boolean): Mood => (busy ? "working" : (flash[agentId] ?? "idle")),
    onMessage: (message: Message) => {
      if (message.role === "error") show(message.agentId, "concerned", CONCERNED_MS);
    },
    onStatus: (agentId: string, busy: boolean) => {
      if (busy) set(agentId, undefined);
      else if (current.get(agentId) !== "concerned") show(agentId, "happy", HAPPY_MS);
    },
  };
}
