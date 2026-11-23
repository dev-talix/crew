import { createSignal } from "solid-js";
import { api, type UpdateCheck } from "./api";

const AUTO_KEY = "grimoire-update-auto";
const LAST_KEY = "grimoire-update-last";
const DAY = 86_400_000;

const [result, setResult] = createSignal<UpdateCheck | null>(null);

/** The latest check, from this launch. */
export { result as updateCheck };

function read(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function write(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // The setting still applies until Grimoire quits.
  }
}

export const autoCheck = () => read(AUTO_KEY) !== "off";
export const setAutoCheck = (on: boolean) => write(AUTO_KEY, on ? "on" : "off");

/** Whether a daily check is due, given when the last one ran. */
export function isDue(last: number | null, now: number): boolean {
  return last === null || !Number.isFinite(last) || now - last >= DAY;
}

export async function checkForUpdates(): Promise<UpdateCheck> {
  const check = await api.checkForUpdates();
  write(LAST_KEY, String(Date.now()));
  setResult(check);
  return check;
}

/** Runs the daily check at launch when it's on. Failures stay quiet; Settings shows them on demand. */
export function startUpdateChecks() {
  const last = read(LAST_KEY);
  if (autoCheck() && isDue(last === null ? null : Number(last), Date.now())) {
    checkForUpdates().catch(() => {});
  }
}
