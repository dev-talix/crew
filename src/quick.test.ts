import { describe, expect, test } from "bun:test";
import { pickAgent, shortcutFromKey, shortcutLabel } from "./quick";

const press = (code: string, mods: { meta?: boolean; ctrl?: boolean; alt?: boolean; shift?: boolean } = {}) => ({
  code,
  metaKey: !!mods.meta,
  ctrlKey: !!mods.ctrl,
  altKey: !!mods.alt,
  shiftKey: !!mods.shift,
});

describe("quick ask shortcuts", () => {
  test("records a key with Command, Control, or Option", () => {
    expect(shortcutFromKey(press("Space", { meta: true, shift: true }))).toBe("Command+Shift+Space");
    expect(shortcutFromKey(press("KeyK", { alt: true }))).toBe("Option+K");
    expect(shortcutFromKey(press("Digit1", { ctrl: true }))).toBe("Control+1");
  });

  test("waits while only modifiers are down and refuses Shift alone", () => {
    expect(shortcutFromKey(press("MetaLeft", { meta: true }))).toBeNull();
    expect(shortcutFromKey(press("ShiftRight", { meta: true, shift: true }))).toBeNull();
    expect(shortcutFromKey(press("KeyA", { shift: true }))).toBeNull();
    expect(shortcutFromKey(press("KeyA"))).toBeNull();
  });

  test("labels shortcuts in macOS order", () => {
    expect(shortcutLabel("CommandOrControl+Shift+Space")).toBe("⇧⌘Space");
    expect(shortcutLabel("Command+Option+Control+K")).toBe("⌃⌥⌘K");
    expect(shortcutLabel("Alt+KeyJ")).toBe("⌥J");
  });

  test("reopens on the last familiar while it still exists", () => {
    expect(pickAgent(["a", "b"], "b")).toBe("b");
    expect(pickAgent(["a", "b"], "gone")).toBe("a");
    expect(pickAgent([], "b")).toBeNull();
  });
});
