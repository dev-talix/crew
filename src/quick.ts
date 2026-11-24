type KeyPress = Pick<KeyboardEvent, "code" | "metaKey" | "ctrlKey" | "altKey" | "shiftKey">;

const MODIFIER_CODES = /^(Meta|Control|Alt|Shift|OS)(Left|Right)?$/;

/**
 * The shortcut a key press makes, in the plugin's format ("Command+Shift+Space"), or null while only
 * modifiers are down or none of Command, Control, or Option is held. Uses the physical key, so
 * Option+K reads as K rather than the character Option makes.
 */
export function shortcutFromKey(e: KeyPress): string | null {
  if (MODIFIER_CODES.test(e.code) || !(e.metaKey || e.ctrlKey || e.altKey)) return null;
  const key = e.code.replace(/^(Key|Digit)(?=.)/, "");
  return [e.metaKey && "Command", e.ctrlKey && "Control", e.altKey && "Option", e.shiftKey && "Shift", key].filter(Boolean).join("+");
}

const SYMBOLS: Record<string, string> = {
  control: "⌃",
  ctrl: "⌃",
  option: "⌥",
  alt: "⌥",
  shift: "⇧",
  command: "⌘",
  cmd: "⌘",
  super: "⌘",
  commandorcontrol: "⌘",
  cmdorctrl: "⌘",
  commandorctrl: "⌘",
  cmdorcontrol: "⌘",
};
const ORDER = ["⌃", "⌥", "⇧", "⌘"];

/** A saved shortcut the way macOS menus write it, like ⇧⌘Space. */
export function shortcutLabel(text: string): string {
  const parts = text.split("+").map((p) => p.trim());
  const key = parts.pop() ?? "";
  const mods = parts.map((p) => SYMBOLS[p.toLowerCase()] ?? p).sort((a, b) => ORDER.indexOf(a) - ORDER.indexOf(b));
  return mods.join("") + key.replace(/^(Key|Digit)(?=.)/, "");
}

/** The familiar Quick Ask opens on: the last one used if it's still there, else the first. */
export function pickAgent(ids: string[], saved: string | null): string | null {
  return saved && ids.includes(saved) ? saved : (ids[0] ?? null);
}
