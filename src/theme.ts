// Turns a theme file into CSS. A theme only needs canvas and accent for an appearance; every other
// role is derived from those two unless the theme sets it.

export const ROLES = [
  "canvas",
  "accent",
  "sidebar",
  "surface",
  "raised",
  "text",
  "textMuted",
  "textFaint",
  "border",
  "accentForeground",
  "userBubble",
  "success",
  "warning",
  "danger",
  "dangerSurface",
] as const;

export type Role = (typeof ROLES)[number];
export type Mode = "dark" | "light";
export type Colors = Partial<Record<Role, string>>;

export interface ThemeFile {
  version?: number;
  name: string;
  colors?: Colors;
  variants?: Partial<Record<Mode, Colors>>;
  css?: string;
  /** The familiar that made it with its tools. */
  createdBy?: string;
}

export const ROLE_LABELS: Record<Role, string> = {
  canvas: "Background",
  accent: "Accent",
  sidebar: "Sidebar",
  surface: "Lists and cards",
  raised: "Inputs and hover",
  text: "Text",
  textMuted: "Secondary text",
  textFaint: "Hints",
  border: "Borders",
  accentForeground: "Text on accent",
  userBubble: "Your messages",
  success: "Success",
  warning: "Warning",
  danger: "Danger",
  dangerSurface: "Error background",
};

const VARS: Record<Role, string> = {
  canvas: "--bg",
  accent: "--accent",
  sidebar: "--sidebar",
  surface: "--surface",
  raised: "--raised",
  text: "--text",
  textMuted: "--muted",
  textFaint: "--faint",
  border: "--line",
  accentForeground: "--accent-ink",
  userBubble: "--bubble",
  success: "--success",
  warning: "--warning",
  danger: "--danger",
  dangerSurface: "--danger-bg",
};

type Rgb = [number, number, number];

function parseHex(hex: string): Rgb {
  let digits = hex.replace("#", "");
  if (digits.length === 3) digits = [...digits].map((d) => d + d).join("");
  return [0, 2, 4].map((i) => parseInt(digits.slice(i, i + 2), 16) / 255) as Rgb;
}

const toHex = (rgb: Rgb) =>
  "#" + rgb.map((c) => Math.round(Math.min(1, Math.max(0, c)) * 255).toString(16).padStart(2, "0")).join("");

const linear = (c: number) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
const gamma = (c: number) => (c <= 0.0031308 ? c * 12.92 : 1.055 * c ** (1 / 2.4) - 0.055);

function toOklab([r, g, b]: Rgb): Rgb {
  const [lr, lg, lb] = [linear(r), linear(g), linear(b)];
  const l = Math.cbrt(0.4122214708 * lr + 0.5363325363 * lg + 0.0514459929 * lb);
  const m = Math.cbrt(0.2119034982 * lr + 0.6806995451 * lg + 0.1073969566 * lb);
  const s = Math.cbrt(0.0883024619 * lr + 0.2817188376 * lg + 0.6299787005 * lb);
  return [
    0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
  ];
}

function fromOklab([L, a, b]: Rgb): Rgb {
  const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s = (L - 0.0894841775 * a - 1.291485548 * b) ** 3;
  return [
    gamma(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s),
    gamma(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s),
    gamma(-0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s),
  ];
}

/** Mixes `amount` of `other` into `base`, in OKLab so the steps look even. */
export function mix(base: string, other: string, amount: number): string {
  const [a, b] = [toOklab(parseHex(base)), toOklab(parseHex(other))];
  return toHex(fromOklab(a.map((v, i) => v + (b[i] - v) * amount) as Rgb));
}

function luminance(hex: string): number {
  const [r, g, b] = parseHex(hex).map(linear);
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

export function contrast(a: string, b: string): number {
  const [x, y] = [luminance(a), luminance(b)].sort((p, q) => q - p);
  return (x + 0.05) / (y + 0.05);
}

/** The colors a theme sets for one appearance. */
export function seeds(theme: ThemeFile, mode: Mode): Colors {
  return { ...theme.colors, ...theme.variants?.[mode] };
}

/** The appearances a theme has both seed colors for. */
export function modesOf(theme: ThemeFile): Mode[] {
  return (["dark", "light"] as Mode[]).filter((mode) => {
    const s = seeds(theme, mode);
    return !!s.canvas && !!s.accent;
  });
}

/** Every role for one appearance: the theme's own colors, with the rest derived from canvas and accent. */
export function palette(theme: ThemeFile, mode: Mode): Record<Role, string> {
  const set = seeds(theme, mode);
  const canvas = set.canvas!;
  const accent = set.accent!;
  const dark = mode === "dark";
  const ink = dark ? "#ffffff" : "#000000";
  const text = set.text ?? mix(dark ? "#f4f2f7" : "#16141c", accent, 0.08);
  const danger = set.danger ?? (dark ? "#f08d7a" : "#c2412d");
  // Dark text on a light accent and white on a dark one, whichever reads better.
  const onAccent = [mix(accent, "#000000", 0.82), "#ffffff"].sort((a, b) => contrast(b, accent) - contrast(a, accent))[0];
  const derived: Record<Role, string> = {
    canvas,
    accent,
    sidebar: dark ? mix(canvas, "#000000", 0.13) : mix(canvas, accent, 0.03),
    surface: dark ? mix(canvas, ink, 0.035) : mix(canvas, "#ffffff", 0.55),
    raised: mix(canvas, dark ? accent : "#000000", dark ? 0.08 : 0.035),
    text,
    textMuted: mix(canvas, text, dark ? 0.58 : 0.66),
    textFaint: mix(canvas, text, dark ? 0.42 : 0.5),
    border: mix(canvas, dark ? text : "#000000", dark ? 0.12 : 0.1),
    accentForeground: onAccent,
    userBubble: mix(canvas, accent, dark ? 0.16 : 0.1),
    success: dark ? "#6fd3a6" : "#1f8a5a",
    warning: dark ? "#e3aa3c" : "#a76b0a",
    danger,
    dangerSurface: mix(canvas, danger, dark ? 0.16 : 0.1),
  };
  for (const role of ROLES) if (set[role]) derived[role] = set[role]!;
  return derived;
}

function block(selector: string, theme: ThemeFile, mode: Mode): string {
  const colors = palette(theme, mode);
  const vars = ROLES.map((role) => `${VARS[role]}:${colors[role]}`).join(";");
  return `${selector}{${vars};color-scheme:${mode}}`;
}

/**
 * CSS for a theme. A theme with both appearances follows the mode setting; one with a single
 * appearance always uses it. The theme's own CSS comes last so it can override anything.
 */
export function themeCss(theme: ThemeFile): string {
  const modes = modesOf(theme);
  const rules =
    modes.length === 2
      ? [
          block(":root", theme, "light"),
          `@media (prefers-color-scheme: dark){${block(':root:not([data-theme="light"])', theme, "dark")}}`,
          block(':root[data-theme="dark"]', theme, "dark"),
        ]
      : modes.map((mode) => block(":root:root", theme, mode));
  return [...rules, theme.css ?? ""].join("\n");
}
