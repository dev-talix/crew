import { createSignal } from "solid-js";
import { api, onEvent, type ThemeEntry, type ThemeList } from "./api";
import { themeCss, type ThemeFile } from "./theme";

export type ModeSetting = "system" | "light" | "dark";

const CSS_KEY = "grimoire-theme-css";
const MODE_KEY = "grimoire-mode";

const [themes, setThemes] = createSignal<ThemeList>();
let current: ThemeList | undefined;
let previewing = false;

/** Every theme, the active id, and files that failed to load. Undefined until the first load. */
export { themes };

export const activeTheme = (list = themes()): ThemeEntry | undefined => list?.themes.find((t) => t.id === list.active);

function styleTag(): HTMLStyleElement {
  let tag = document.getElementById("theme") as HTMLStyleElement | null;
  if (!tag) {
    tag = document.createElement("style");
    tag.id = "theme";
    document.head.append(tag);
  }
  return tag;
}

function store(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Storage can be unavailable. The theme still applies; it just isn't cached for the next launch.
  }
}

function read(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function applyActive() {
  const entry = current?.themes.find((t) => t.id === current!.active);
  if (!entry || previewing) return;
  const css = themeCss(entry.theme);
  styleTag().textContent = css;
  // Cached so the next launch paints in this theme before the list loads.
  store(CSS_KEY, css);
}

/** Shows `theme` without saving it, or goes back to the active theme with `null`. */
export function previewTheme(theme: ThemeFile | null) {
  previewing = theme !== null;
  if (theme) styleTag().textContent = themeCss(theme);
  else applyActive();
}

export async function reloadThemes(): Promise<void> {
  try {
    current = await api.listThemes();
    setThemes(current);
    applyActive();
  } catch {
    // Keep the theme on screen. Settings shows the list once loading works again.
  }
}

export function readMode(): ModeSetting {
  const stored = read(MODE_KEY);
  return stored === "light" || stored === "dark" ? stored : "system";
}

export function setMode(mode: ModeSetting) {
  store(MODE_KEY, mode);
  applyMode(mode);
}

/** Shows one appearance while the theme editor works on it, or goes back to the setting with `null`. */
export function previewMode(mode: "light" | "dark" | null) {
  applyMode(mode ?? readMode());
}

/** The appearance on screen now. */
export function shownMode(): "light" | "dark" {
  const forced = document.documentElement.dataset.theme;
  if (forced === "light" || forced === "dark") return forced;
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

function applyMode(mode: ModeSetting) {
  if (mode === "system") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = mode;
}

/** Applies the cached theme at once, then loads the real list and follows changes from any window. */
export function startThemes() {
  const cached = read(CSS_KEY);
  if (cached) styleTag().textContent = cached;
  applyMode(readMode());
  window.addEventListener("storage", (event) => {
    if (event.key === MODE_KEY) applyMode(readMode());
  });
  // Theme files can change in a text editor, so look again whenever the window comes back.
  window.addEventListener("focus", () => void reloadThemes());
  void onEvent("themes_changed", () => void reloadThemes());
  void reloadThemes();
}
