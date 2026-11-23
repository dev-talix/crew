import type { Glyph } from "../glyphs";
import { lineOnly, outlined } from "../strokes";

// Closed grimoire standing upright: the page block peeks out right and below the cover, a spine with
// head and tail bands on the left, and a bookmark ribbon hanging below the page block.
// Every part sits outside the others' outlines so no stroke crosses a tinted fill.
// Leaves and the sparkle stay hidden until the working and happy moods show them.
export const tome: Glyph = {
  body: (s) =>
    `<g class="g-tome-tail"><path class="g-tome-ribbon" d="M35 59v7l2-1.8 2 1.8v-7" ${outlined(s)}/></g>` +
    `<path d="M46 11h5a3 3 0 0 1 3 3v42a3 3 0 0 1-3 3H17a3 3 0 0 1-3-3v-1H46z" fill="${s.color}" fill-opacity="0.12"/>` +
    `<path d="M46 11h5a3 3 0 0 1 3 3v42a3 3 0 0 1-3 3H17a3 3 0 0 1-3-3v-1" ${lineOnly(s)}/>` +
    `<path d="M50 14.5V55H18" ${lineOnly(s, 0.45)}/>` +
    `<path class="g-tome-leaf" d="M54 21q3-3.5 7-3" ${lineOnly(s, 0.85)}/>` +
    `<path class="g-tome-leaf" d="M54 41q3-3.5 7-3" ${lineOnly(s, 0.85)}/>` +
    `<path class="g-tome-leaf" d="M54 48q3-3.5 7-3" ${lineOnly(s, 0.85)}/>` +
    `<g class="g-tome-cover"><rect x="10" y="7" width="36" height="48" rx="3.5" ${outlined(s)}/>` +
    `<path d="M16 7v48M10 12.5h6M10 49.5h6" ${lineOnly(s, 0.6)}/></g>` +
    `<path class="g-tome-sparkle" d="M57 1v8M53 5h8" ${lineOnly(s)}/>`,
  eyes: { left: 25.5, right: 36.5, y: 30, scale: 1 },
};
