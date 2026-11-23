import type { Shape } from "./avatar";

export interface GlyphStyle {
  color: string;
  stroke: number;
}

export interface EyeAnchor {
  left: number;
  right: number;
  y: number;
  scale: number;
}

export interface Glyph {
  body: (s: GlyphStyle) => string;
  eyes: EyeAnchor;
}

const outlined = (s: GlyphStyle) =>
  `stroke="${s.color}" stroke-width="${s.stroke}" vector-effect="non-scaling-stroke" stroke-linecap="round" stroke-linejoin="round" fill="${s.color}" fill-opacity="0.12"`;

const lineOnly = (s: GlyphStyle, opacity = 1) =>
  `stroke="${s.color}" stroke-opacity="${opacity}" stroke-width="${s.stroke}" vector-effect="non-scaling-stroke" stroke-linecap="round" stroke-linejoin="round" fill="none"`;

const moon = (s: GlyphStyle, begin: string) => {
  const timing = `dur="8s" begin="${begin}" repeatCount="indefinite"`;
  const keys = `keyTimes="0;0.5;0.56;0.615;0.633;0.867;0.885;0.94;1" ${timing}`;
  return `<circle r="2.6" fill="${s.color}"><animateMotion path="M59 32A27 9 0 1 1 5 32A27 9 0 1 1 59 32" ${timing}/><animate attributeName="opacity" values="1;1;0.6;0.6;0;0;0.6;0.6;1" ${keys}/><animate attributeName="r" values="2.6;2.6;1.8;1.8;1.8;1.8;1.8;1.8;2.6" ${keys}/></circle>`;
};

export const GLYPHS: Record<Shape, Glyph> = {
  orbit: {
    body: (s) =>
      // The ring is split at the planet's edge so its back half sits behind the planet. Moons travel
      // the ring in its own plane, shrinking and hiding while they pass behind (path fractions 0.633-0.867).
      `<g transform="rotate(-18 32 32)"><path d="M5 32A27 9 0 0 1 17.97 24.31M46.03 24.31A27 9 0 0 1 59 32" ${lineOnly(s, 0.3)}/><circle cx="32" cy="32" r="16" ${outlined(s)}/><path d="M59 32A27 9 0 0 1 5 32" ${lineOnly(s, 0.6)}/>${moon(s, "0s")}<g class="g-moon2">${moon(s, "-4s")}</g></g>`,
    eyes: { left: 26.5, right: 37.5, y: 31, scale: 0.8 },
  },
  spark: {
    body: (s) =>
      `<path d="M32 6c2.5 12 9 19 24 26-15 7-21.5 14-24 26-2.5-12-9-19-24-26 15-7 21.5-14 24-26z" ${outlined(s)}/><path class="g-glint" d="M51 9v7M47.5 12.5h7" ${lineOnly(s)}/><path class="g-glint" d="M13 48v5M10.5 50.5h5" ${lineOnly(s)}/>`,
    eyes: { left: 28, right: 36, y: 32, scale: 0.7 },
  },
  cloud: {
    body: (s) =>
      `<path d="M19 50a11 11 0 0 1-1-22 15 15 0 0 1 29-3 12 12 0 0 1-1 25z" ${outlined(s)}/><path class="g-rain" d="M24 55v3" ${lineOnly(s)}/><path class="g-rain" d="M32 56v3" ${lineOnly(s)}/><path class="g-rain" d="M40 55v3" ${lineOnly(s)}/>`,
    eyes: { left: 26, right: 38, y: 38, scale: 1 },
  },
  drop: {
    body: (s) => `<path d="M32 7c9 11 20 19 20 32a20 20 0 0 1-40 0c0-13 11-21 20-32z" ${outlined(s)}/>`,
    eyes: { left: 25, right: 39, y: 39, scale: 1 },
  },
  ears: {
    body: (s) =>
      `<path class="g-ear-l" d="M13 19 12 8l10 8" ${lineOnly(s)}/><path class="g-ear-r" d="M51 19l1-11-10 8" ${lineOnly(s)}/><rect x="9" y="16" width="46" height="39" rx="16" ${outlined(s)}/>`,
    eyes: { left: 24.5, right: 39.5, y: 35, scale: 1 },
  },
  classic: {
    body: (s) =>
      `<g class="g-antenna"><path d="M32 8V3" ${lineOnly(s)}/><circle class="g-bulb" cx="32" cy="2.5" r="2.5" fill="${s.color}"/></g><rect x="14" y="8" width="36" height="30" rx="10" ${outlined(s)}/><path d="M32 38v6M12 59a20 13 0 0 1 40 0" ${lineOnly(s)}/>`,
    eyes: { left: 25, right: 39, y: 23, scale: 1 },
  },
};
