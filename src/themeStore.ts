import { createSignal } from "solid-js";
import { api, onEvent, type ThemeEntry, type ThemeList } from "./api";
import { themeCss, type ThemeFile } from "./theme";

export type ModeSetting = "system" | "light" | "dark";
export type AppIconSetting = "auto" | "paper" | "night";

const CSS_KEY = "grimoire-theme-css";
const MODE_KEY = "grimoire-mode";
const ICON_KEY = "grimoire-app-icon";

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

// The theme CSS sets color-scheme, so this also covers themes that only have one appearance.
const readAppearance = (): "light" | "dark" =>
  getComputedStyle(document.documentElement).colorScheme.includes("dark") ? "dark" : "light";
const [appearance, setAppearance] = createSignal<"light" | "dark">("dark");
const refreshAppearance = () => setAppearance(readAppearance());

/** The appearance on screen now, after the mode setting and the theme. */
export { appearance };

const readIcon = (): AppIconSetting => {
  const stored = read(ICON_KEY);
  return stored === "paper" || stored === "night" ? stored : "auto";
};
const [appIcon, setAppIconSignal] = createSignal<AppIconSetting>(readIcon());

/** The app icon setting, and the icon it resolves to: night on a dark appearance when set to auto. */
export { appIcon };
export const shownIcon = () => {
  const choice = appIcon();
  return choice === "auto" ? (appearance() === "dark" ? "night" : "paper") : choice;
};

export function setAppIcon(choice: AppIconSetting) {
  store(ICON_KEY, choice);
  setAppIconSignal(choice);
}

function applyActive() {
  const entry = current?.themes.find((t) => t.id === current!.active);
  if (!entry || previewing) return;
  const css = themeCss(entry.theme);
  styleTag().textContent = css;
  refreshAppearance();
  // Cached so the next launch paints in this theme before the list loads.
  store(CSS_KEY, css);
}

/** Shows `theme` without saving it, or goes back to the active theme with `null`. */
export function previewTheme(theme: ThemeFile | null) {
  previewing = theme !== null;
  if (theme) {
    styleTag().textContent = themeCss(theme);
    refreshAppearance();
  } else applyActive();
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

function applyMode(mode: ModeSetting) {
  if (mode === "system") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = mode;
  refreshAppearance();
}

/** Applies the cached theme at once, then loads the real list and follows changes from any window. */
export function startThemes() {
  const cached = read(CSS_KEY);
  if (cached) styleTag().textContent = cached;
  applyMode(readMode());
  window.addEventListener("storage", (event) => {
    if (event.key === MODE_KEY) applyMode(readMode());
    if (event.key === ICON_KEY) setAppIconSignal(readIcon());
  });
  window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", refreshAppearance);
  // Theme files can change in a text editor, so look again whenever the window comes back.
  window.addEventListener("focus", () => void reloadThemes());
  void onEvent("themes_changed", () => void reloadThemes());
  void reloadThemes();
}
