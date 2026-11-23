import { expect, test } from "bun:test";
import { contrast, mix, modesOf, palette, ROLES, themeCss, type ThemeFile } from "./theme";

const both: ThemeFile = {
  name: "Both",
  variants: { dark: { canvas: "#1b1922", accent: "#9b7bff" }, light: { canvas: "#f7f5fa", accent: "#6a48e0" } },
};

test("mixing returns the ends at 0 and 1", () => {
  expect(mix("#102030", "#ffffff", 0)).toBe("#102030");
  expect(mix("#102030", "#ffffff", 1)).toBe("#ffffff");
});

test("a palette derives every role and keeps the theme's own colors", () => {
  const theme: ThemeFile = { ...both, colors: { sidebar: "#000000" } };
  const dark = palette(theme, "dark");
  for (const role of ROLES) expect(dark[role]).toMatch(/^#[0-9a-f]{6}$/);
  expect(dark.sidebar).toBe("#000000");
  expect(dark.canvas).toBe("#1b1922");
  // Derived text stays readable on the background in both appearances.
  expect(contrast(dark.text, dark.canvas)).toBeGreaterThan(12);
  const light = palette(theme, "light");
  expect(contrast(light.text, light.canvas)).toBeGreaterThan(12);
  expect(contrast(light.textMuted, light.canvas)).toBeGreaterThan(4.5);
});

test("text on the accent picks whichever of dark or white reads better", () => {
  const onGold = palette({ name: "Gold", colors: { canvas: "#1b1914", accent: "#e2b84f" } }, "dark").accentForeground;
  const onBlue = palette({ name: "Blue", colors: { canvas: "#f4f5fc", accent: "#2f3fc4" } }, "light").accentForeground;
  expect(onBlue).toBe("#ffffff");
  expect(contrast(onGold, "#e2b84f")).toBeGreaterThan(7);
});

test("a theme with one appearance always uses it", () => {
  const night: ThemeFile = { name: "Night", variants: { dark: { canvas: "#000000", accent: "#8a7fe8" } }, css: ".x{}" };
  expect(modesOf(night)).toEqual(["dark"]);
  const css = themeCss(night);
  expect(css).not.toContain("prefers-color-scheme");
  expect(css).toContain("color-scheme:dark");
  expect(css.endsWith(".x{}")).toBe(true);
  expect(modesOf(both)).toEqual(["dark", "light"]);
  expect(themeCss(both)).toContain("prefers-color-scheme: dark");
});
