import type { Glyph } from "../glyphs";
import { lineOnly, outlined } from "../strokes";

// A stubby wax candle on a saucer. The flame sits in two groups: the outer one leans, flares and
// shrinks to an ember, the inner one flickers, so a held pose and a loop never fight over transform.
export const candle: Glyph = {
  body: (s) =>
    `<path class="g-candle-smoke" d="M32 15c-2.5-2 2.5-4 0-6.5s2.5-4 0-6.5" ${lineOnly(s, 0.45)}/>` +
    `<path class="g-candle-spark" d="M20 12l-3-2.5" ${lineOnly(s)}/>` +
    `<path class="g-candle-spark" d="M44 12l3-2.5" ${lineOnly(s)}/>` +
    `<path d="M32 25.5v2.5" ${lineOnly(s)}/>` +
    `<g class="g-candle-flame"><path class="g-candle-fire" d="M32 4c3.2 5 6.8 8.6 6.8 13.4a6.8 6.8 0 0 1-13.6 0C25.2 12.6 28.8 9 32 4z" ${outlined(s)}/></g>` +
    `<path d="M19 31q0-3 3-3h20q3 0 3 3v23q0 3-3 3H22q-3 0-3-3z" ${outlined(s)}/>` +
    `<path class="g-candle-drip" d="M23 28.5v6a1.6 1.6 0 0 0 3.2 0v-6" ${lineOnly(s, 0.7)}/>` +
    `<path d="M12 57c1 2.6 3 3.6 6 3.6h28c3 0 5-1 6-3.6" ${lineOnly(s)}/>`,
  eyes: { left: 27, right: 37.5, y: 44, scale: 0.85 },
};
