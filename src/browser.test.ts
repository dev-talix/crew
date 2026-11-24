import { describe, expect, test } from "bun:test";
import { keyInput, toPagePoint } from "./browser";

const press = (key: string, mods: { meta?: boolean; ctrl?: boolean; alt?: boolean; shift?: boolean } = {}) => ({
  key,
  metaKey: !!mods.meta,
  ctrlKey: !!mods.ctrl,
  altKey: !!mods.alt,
  shiftKey: !!mods.shift,
});

describe("browser view clicks", () => {
  const rect = { left: 100, top: 50, width: 640, height: 400 };

  test("maps the scaled view onto the page", () => {
    expect(toPagePoint(100, 50, rect, 1280, 800)).toEqual({ x: 0, y: 0 });
    expect(toPagePoint(420, 250, rect, 1280, 800)).toEqual({ x: 640, y: 400 });
    expect(toPagePoint(100.3, 50.3, rect, 1280, 800)).toEqual({ x: 1, y: 1 });
  });

  test("keeps points inside the page", () => {
    expect(toPagePoint(0, 0, rect, 1280, 800)).toEqual({ x: 0, y: 0 });
    expect(toPagePoint(2000, 2000, rect, 1280, 800)).toEqual({ x: 1279, y: 799 });
  });
});

describe("browser view keys", () => {
  test("types printable characters", () => {
    expect(keyInput(press("a"))).toEqual({ kind: "text", text: "a" });
    expect(keyInput(press("A", { shift: true }))).toEqual({ kind: "text", text: "A" });
    expect(keyInput(press(" "))).toEqual({ kind: "text", text: " " });
    expect(keyInput(press("å", { alt: true }))).toEqual({ kind: "text", text: "å" });
  });

  test("sends named keys with their modifiers", () => {
    expect(keyInput(press("Enter"))).toEqual({ kind: "key", key: "Enter", modifiers: 0 });
    expect(keyInput(press("Tab", { shift: true }))).toEqual({ kind: "key", key: "Tab", modifiers: 8 });
    expect(keyInput(press("ArrowLeft", { alt: true, meta: true }))).toEqual({ kind: "key", key: "ArrowLeft", modifiers: 5 });
  });

  test("sends shortcuts as lowercase letters, except paste", () => {
    expect(keyInput(press("a", { meta: true }))).toEqual({ kind: "key", key: "a", modifiers: 4 });
    expect(keyInput(press("Z", { meta: true, shift: true }))).toEqual({ kind: "key", key: "z", modifiers: 12 });
    expect(keyInput(press("c", { ctrl: true }))).toEqual({ kind: "key", key: "c", modifiers: 2 });
    expect(keyInput(press("v", { meta: true }))).toBeNull();
    expect(keyInput(press("v", { ctrl: true }))).toBeNull();
  });

  test("leaves other keys alone", () => {
    expect(keyInput(press("Shift", { shift: true }))).toBeNull();
    expect(keyInput(press("F5"))).toBeNull();
    expect(keyInput(press(",", { meta: true }))).toBeNull();
    expect(keyInput(press("1", { ctrl: true }))).toBeNull();
  });
});
