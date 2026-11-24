import { createSignal } from "solid-js";
import { api, type UpdateCheck } from "./api";

const AUTO_KEY = "grimoire-update-auto";
const LAST_KEY = "grimoire-update-last";
const DAY = 86_400_000;

const [result, setResult] = createSignal<UpdateCheck | null>(null);
const [installing, setInstalling] = createSignal(false);
// The version installed this launch, which runs after a restart.
const [installed, setInstalled] = createSignal<string | null>(null);

/** The latest check, from this launch. */
export { result as updateCheck, installing as updateInstalling, installed as updateInstalled };

function read(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function forget(key: string) {
  try {
    localStorage.removeItem(key);
  } catch {
    // The next launch checks again anyway when storage is unavailable.
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

/** Whether to install a found update without asking: automatic updates are on and this build can. */
export function shouldInstall(check: UpdateCheck | null, auto: boolean): boolean {
  return auto && !!check?.newer && check.canInstall;
}

/** Downloads, verifies, and installs the newest release. It runs after the next restart. */
export async function installUpdate(): Promise<string | null> {
  if (installing()) return null;
  setInstalling(true);
  try {
    const version = await api.installUpdate();
    if (version) setInstalled(version);
    return version;
  } finally {
    setInstalling(false);
  }
}

export const restartToUpdate = () => api.restartApp();

/**
 * Runs the daily check at launch when it's on, and installs what it finds when the build can.
 * Failures stay quiet; Settings shows them on demand.
 */
export function startUpdateChecks() {
  const last = read(LAST_KEY);
  if (autoCheck() && isDue(last === null ? null : Number(last), Date.now())) {
    checkForUpdates()
      .then((check) =>
        shouldInstall(check, autoCheck())
          ? // A failed download shouldn't wait a day: the next launch checks and tries again.
            installUpdate().catch(() => forget(LAST_KEY))
          : null,
      )
      .catch(() => {});
  }
}
