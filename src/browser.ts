import type { BrowserInput } from "./api";

/** Keys the browser accepts by name. */
const NAMED_KEYS = new Set([
  "Enter",
  "Backspace",
  "Tab",
  "Escape",
  "Delete",
  "ArrowUp",
  "ArrowDown",
  "ArrowLeft",
  "ArrowRight",
  "Home",
  "End",
  "PageUp",
  "PageDown",
]);

type KeyPress = { key: string; altKey: boolean; ctrlKey: boolean; metaKey: boolean; shiftKey: boolean };

/** The page point under a click on the live view, rounded and kept inside the page. */
export function toPagePoint(
  clientX: number,
  clientY: number,
  rect: { left: number; top: number; width: number; height: number },
  pageWidth: number,
  pageHeight: number,
): { x: number; y: number } {
  const clamp = (n: number, max: number) => Math.min(Math.max(Math.round(n), 0), Math.max(max - 1, 0));
  return {
    x: clamp(((clientX - rect.left) / (rect.width || 1)) * pageWidth, pageWidth),
    y: clamp(((clientY - rect.top) / (rect.height || 1)) * pageHeight, pageHeight),
  };
}

/** What a key press sends to the browser, or null to leave it alone. ⌘V and Ctrl+V come through the paste event. */
export function keyInput(e: KeyPress): BrowserInput | null {
  const modifiers = (e.altKey ? 1 : 0) | (e.ctrlKey ? 2 : 0) | (e.metaKey ? 4 : 0) | (e.shiftKey ? 8 : 0);
  const command = e.ctrlKey || e.metaKey;
  if ([...e.key].length === 1 && !command) return { kind: "text", text: e.key };
  if (NAMED_KEYS.has(e.key)) return { kind: "key", key: e.key, modifiers };
  if (command && /^[a-z]$/i.test(e.key)) {
    const key = e.key.toLowerCase();
    return key === "v" ? null : { kind: "key", key, modifiers };
  }
  return null;
}
