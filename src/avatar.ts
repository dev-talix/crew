export type Shape = "orbit" | "spark" | "cloud" | "drop" | "ears" | "classic";
export type Eyes = "pill" | "dot" | "slit";

export interface Avatar {
  shape: Shape;
  eyes: Eyes;
  color: string;
}

export interface PartOption<T> {
  value: T;
  label: string;
}

export const SHAPES: PartOption<Shape>[] = [
  { value: "orbit", label: "Orbit" },
  { value: "spark", label: "Spark" },
  { value: "cloud", label: "Cloud" },
  { value: "drop", label: "Drop" },
  { value: "ears", label: "Ears" },
  { value: "classic", label: "Classic" },
];

export const EYES: PartOption<Eyes>[] = [
  { value: "pill", label: "Pills" },
  { value: "dot", label: "Dots" },
  { value: "slit", label: "Slits" },
];

export const PALETTE: PartOption<string>[] = [
  { value: "#4c7ef3", label: "Azure" },
  { value: "#6266e8", label: "Indigo" },
  { value: "#8e5bd6", label: "Plum" },
  { value: "#d9577a", label: "Rose" },
  { value: "#e46f4c", label: "Coral" },
  { value: "#dc9a32", label: "Amber" },
  { value: "#2f9e76", label: "Jade" },
  { value: "#2896a6", label: "Teal" },
  { value: "#667180", label: "Slate" },
  { value: "#333945", label: "Graphite" },
]

const pick = <T,>(options: PartOption<T>[]): T => options[Math.floor(Math.random() * options.length)].value;

const BRIGHT = PALETTE.slice(0, 8);

export function randomAvatar(): Avatar {
  return { shape: pick(SHAPES), eyes: pick(EYES), color: pick(BRIGHT) };
}

function channels(hex: string): [number, number, number] {
  const value = parseInt(hex.slice(1), 16);
  return [(value >> 16) & 255, (value >> 8) & 255, value & 255];
}

function toHex([r, g, b]: number[]): string {
  return `#${[r, g, b].map((c) => Math.round(Math.min(255, Math.max(0, c))).toString(16).padStart(2, "0")).join("")}`;
}

export function luminance(hex: string): number {
  const [r, g, b] = channels(hex).map((c) => {
    const s = c / 255;
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

export function inkFor(hex: string): string {
  const onDark = (1.05) / (luminance(hex) + 0.05);
  const onLight = (luminance(hex) + 0.05) / (luminance("#1b1e24") + 0.05);
  return onDark > onLight ? "#ffffff" : "#1b1e24";
}

export function shade(hex: string, amount: number): string {
  const target = amount < 0 ? 0 : 255;
  return toHex(channels(hex).map((c) => c + (target - c) * Math.abs(amount)));
}

export function isHexColor(value: string): boolean {
  return /^#[0-9a-f]{6}$/i.test(value);
}
