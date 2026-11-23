import type { Glyph } from "../glyphs";
import { lineOnly, outlined } from "../strokes";

// A crystal ball (center 32,30, r 19) on a footed stand. The stand's tint stops at the ball's bottom arc
// and its stroke stops at the ball's edge, so nothing doubles up where they meet. The flash ring is open
// at the bottom so it never crosses the stand. Mist and motes stay 12-16 units from the center, clear of the eyes.
export const crystal: Glyph = {
  body: (s) =>
    `<path d="M21.5 45.84 16.6 55a2 2 0 0 0 1.8 3h27.2a2 2 0 0 0 1.8-3l-4.9-9.16A19 19 0 0 1 21.5 45.84z" fill="${s.color}" fill-opacity="0.12"/>` +
    `<path d="M21.5 45.84 16.6 55a2 2 0 0 0 1.8 3h27.2a2 2 0 0 0 1.8-3l-4.9-9.16" ${lineOnly(s)}/>` +
    `<path class="g-crystal-flash" d="M15.84 40A19 19 0 1 1 48.16 40" ${lineOnly(s, 0.7)}/>` +
    `<g class="g-crystal-glass"><circle cx="32" cy="30" r="19" ${outlined(s)}/></g>` +
    `<path class="g-crystal-mist" d="M18 30a14 14 0 0 1 12-13.9q4-.1 5 2.9M46 30a14 14 0 0 1-12 13.9q-4 .1-5-2.9" ${lineOnly(s, 0.32)}/>` +
    `<g class="g-crystal-motes"><circle cx="32" cy="15.5" r="1.4" fill="${s.color}"/><circle cx="32" cy="44.5" r="1.4" fill="${s.color}"/></g>` +
    `<path class="g-crystal-fog" d="M20 39q3-2.2 6 0t6 0 6 0 6 0" ${lineOnly(s, 0.5)}/>` +
    `<path class="g-crystal-glint" d="M19.5 23a14.5 14.5 0 0 1 6-6.6" ${lineOnly(s, 0.75)}/>` +
    `<path class="g-crystal-crack" d="M44 18.5l-3.2 3.2 2.4 1.8-2.6 3" ${lineOnly(s, 0.9)}/>`,
  eyes: { left: 25.5, right: 38.5, y: 31, scale: 0.9 },
};
